# DOC-30 — Project Manager: Vision & Lifecycle Orchestrator

**Status:** DRAFT — Vision & Design Work Only (0% implemented)
**Subsystem:** trusty-mpm — project-level orchestration / user-facing surface
**Owner:** Product / Engineering (trusty-mpm)
**Last-updated:** 2026-07-01
**Spec ID:** `SPEC-PM-01~draft` (DOC-30)
**Builds on:** DOC-17 — Autonomous Multi-Session Managed Harness Runner (`docs/specs/harness-runner-vision.md`); DOC-14 — Session Manager (SM) Agent (`docs/specs/session-manager-agent.md`); DOC-22 — Multi-Project / Multi-Repo Aware Session Manager (`docs/specs/multi-repo-session-routing.md`); SESSION_MANAGER_MVP.md (`docs/trusty-mpm/spec/SESSION_MANAGER_MVP.md`); DOC-29 — Primary trusty-mpm Harness Behaviors (`docs/specs/mpm-behavior-conformance.md`)
**Cross-ref:** epics **#1045** (metaharness runner-core), **#1272** (interactive-TUI), **#1517** (multi-project awareness), **#1526** (autonomy learning), **#832** (multi-repo); the session-manager core (`crates/trusty-mpm/src/session_manager/`), the routing layer (`crates/trusty-mpm/src/session_manager/`), memory integration (`crates/trusty-memory`), and related vision specs (DOC-17, DOC-22, DOC-23)

> **Scope note.** This is a **vision and architectural boundary** spec. It describes a
> **NEW orchestration layer** that sits **ABOVE** the existing Session Manager (DOC-14),
> providing the primary user-facing interface and owning the full project lifecycle: deliverables,
> estimations, milestone tracking, spec linkage, session orchestration, and status management.
> **This spec is DESIGN-ONLY: it proposes the boundary, data model, and user interaction surface
> without implementing code.** The Session Manager continues to own tmux/session mechanics unchanged;
> the Project Manager is the orchestrator that USES Session Manager as a tool for session
> lifecycle. A future phase will implement Project Manager behaviors (SPEC-PM-01 through SPEC-PM-XX)
> and add them to DOC-29's conformance matrix. This spec defines **what** the Project Manager is and
> **why**; the **how** is deferred to phase planning and implementation.

---

## 1. Motivation & Problem

### The status quo: Session-centric, not project-centric

Today, trusty-mpm's user-facing surface (CLI, TUI, Telegram) is **session-centric**:
- A user spawns `tm sessions new --repo <url> --task "<desc>"` — directly creating a session.
- The system tracks sessions by ID, state, and pane output.
- Sessions are independent — there is no grouping, no estimations, no delivery tracking across sessions.
- To understand "what is the status of project X?" a user must manually correlate sessions by repo and reason about them.

### The vision: Project Manager as the primary proxy

Bob's directive: **"The Project Manager is the MAIN PROXY for the user — the primary way a user
will eventually interact with the harness system overall (not raw session mechanics)."**

This means:
1. **User intent is project-scoped, not session-scoped.** Users think "I need to ship feature X for project Y by date Z" — not "I need to spawn 3 sessions for this."
2. **Projects have lifecycles.** A project has deliverables, milestones, estimations, spec linkages, and an overall status — all of which roll up to the user's delivery commitments.
3. **Sessions are a resource, not a contract.** The Project Manager creates and orchestrates sessions via Session Manager as needed, but the user-facing contract is the project, not the session.
4. **Coding happens in sessions, but project management happens in Project Manager.** Decisions about *what* to build, *when* it's due, *how* well we estimated, and *whether* we delivered are Project Manager concerns — not Session Manager concerns.

---

## 2. Architectural Boundary

### 2.1 Project Manager scope (NEW layer)

Project Manager **OWNS**:
- Project definition and registration (name, description, key dates, stakeholders)
- Deliverable tracking (discrete units of work — features, fixes, refactors — with scope descriptions)
- Milestone definition (projected delivery dates, grouping of deliverables)
- Estimation (effort estimates, burn-down, velocity metrics)
- Spec linkage (which specs are being implemented by this project, version tracking)
- Session orchestration API (spawn sessions on behalf of a project, track which sessions serve the project)
- Project status (proposed → in-progress → blocked → delivered → shipped)
- User-facing surfaces (CLI `tm project`, TUI, Telegram, Web dashboard)
- Handoff protocol with Session Manager (creating sessions with project context, receiving session-completion signals)

Project Manager does **NOT** do:
- **Write or edit code.** That happens inside sessions managed by Session Manager.
- **Understand tmux.** Session mechanics are Session Manager's domain.
- **Manage individual session I/O.** `send_input`, `capture_pane`, `attach` commands are Session Manager's.
- **Replace Session Manager.** Session Manager remains the durable substrate for tmux session lifecycle.

### 2.2 Session Manager scope (EXISTING layer, UNCHANGED)

Session Manager continues to **own**:
- Session lifecycle (create, monitor, send input, capture output, stop, decommission)
- Workspace provisioning (clone repo, run `prepare_session`)
- tmux integration (spawn Claude Code, manage panes)
- Activity observation (pane capture, LLM activity monitor)
- Pending-decision exposure and injection
- Multi-repo routing (NL→repo resolution, project registry) — *coordinated with Project Manager*

Session Manager is **harness-agnostic**: it works with Claude Code, trusty-code, or future runtimes.

### 2.3 Relationship to multi-repo routing (DOC-22)

**DECIDED (Decision #11):** DOC-22's NL-to-repo resolver becomes an INTERNAL INPUT MECHANISM for Project Manager, not a separate competing entry point. A user's natural-language task is resolved to a Project (existing or new) **via DOC-22's resolver**, making Project Manager the primary surface and DOC-22 a subordinate routing layer. This is hierarchical: Project Manager is the main user proxy, Session Manager owns session mechanics, and DOC-22's resolver feeds Project Manager, not the other way around.

---

## 3. Core Concepts & Data Model

### 3.1 Project

A **Project** is a tracked unit of work anchored to a repository and a deadline.

```
Project {
  id: ProjectId (UUID),
  name: String (human-readable, unique),
  description: String,
  repo_url: String (GitHub URL),
  owner_user: String (who created/owns it),
  default_branch: String (default ref for sessions, e.g., "main"),
  status: ProjectStatus (proposed | in-progress | blocked | delivered | shipped),
  created_at: DateTime,
  target_delivery_date: Option<DateTime> (when the user wants it shipped),
  milestones: Vec<MilestoneRef>,
  deliverables: Vec<DeliverableRef>,
  spec_refs: Vec<SpecRef> (linked docs/specs/ files),
  notes: String (narrative project context),
}
```

**DECIDED (Decision #1):** Each Project is **1:1 with a git repository**. A monorepo (like trusty-tools itself) is ONE Project; finer granularity is expressed via Deliverables, not separate Projects.

### 3.2 Deliverable

A **Deliverable** is a discrete unit of work within a Project.

```
Deliverable {
  id: DeliverableId (UUID),
  project_id: ProjectId,
  name: String (e.g., "OAuth2 authentication flow"),
  description: String (scope statement),
  type: DeliverableType (feature | bugfix | refactor | chore | test | docs),
  ticket_ref: Option<String> (e.g., "GH-1234", "JIRA-567"),
  spec_ref: Option<SpecRef> (which docs/specs/*.md is this implementing?),
  status: DeliverableStatus (proposed | in-progress | blocked | complete | delivered),
  estimated_effort: EstimationTier (S | M | L | XL),  // Tier-based, not hours/ranges
  created_at: DateTime,
  target_date: Option<DateTime>,
  assigned_to: Option<String> (which harness/agent is working on this?),
}
```

**DECIDED (Decision #2):** Estimation uses **tiers (S/M/L/XL)**, not point-hours or ranges. This is coarse-grained and avoids false precision.

**DECIDED (Decision #3):** Deliverables are **flat — no recursive sub-tasks**. Hierarchy can be added later if flat proves insufficient; the MVP keeps it simple.

### 3.3 Milestone

A **Milestone** groups Deliverables and provides a delivery checkpoint.

```
Milestone {
  id: MilestoneId (UUID),
  project_id: ProjectId,
  name: String (e.g., "v1.0 Alpha", "Q3 Launch"),
  description: String,
  target_date: DateTime,
  status: MilestoneStatus (proposed | in-progress | complete | shipped),
  deliverables: Vec<DeliverableId>,
  created_at: DateTime,
}
```

### 3.4 Session-Project Binding

Project Manager tracks which sessions are working on which Deliverables.

```
SessionBinding {
  session_id: SessionId (from Session Manager),
  project_id: ProjectId,
  deliverable_id: Option<DeliverableId> (which deliverable, if known),
  started_at: DateTime,
  status: SessionBindingStatus (active | paused | complete | abandoned),
  // Session Manager provides repo_url + branch; Project Manager adds project context
}
```

**DECIDED (Decision #7):** Binding is **1 Deliverable ↔ many Sessions** (not strict 1:1, not full N:M). Each session works on exactly ONE deliverable at a time, but a Deliverable can accumulate multiple sessions over its life (first attempt, review-fix follow-up, etc.).

### 3.5 Spec Reference

A **SpecRef** links a Project or Deliverable to a docs/specs/ file (e.g., DOC-17, SPEC-PM-01).

```
SpecRef {
  doc_id: String (e.g., "DOC-17", "SPEC-PM-01"),
  file_path: String (e.g., "docs/specs/harness-runner-vision.md"),
  version: Option<String> (for tracking which version of the spec this project implements),
  implemented: bool (is this spec fully implemented, partially, or planned?),
}
```

---

## 4. User Interaction Model

### 4.1 The primary user interface

**DECIDED (Decision #4):** **CLI first** (`tm project` namespace), matching how `tm` already works. TUI (epic #1272) and Telegram/web come later.

```
tm project list                         # List all projects
tm project create --name "feature-x" --repo <url> --due-date <date>
tm project show <project-id>            # Show project + deliverables + milestones + sessions
tm project add-deliverable <project-id> --name "..." --type feature --estimate S|M|L|XL
tm project add-milestone <project-id> --name "v1.0" --target-date <date>
tm project update <project-id> --status in-progress  # or blocked, delivered, shipped, etc.
tm project spawn-session <project-id> --deliverable <id> --task "..."
tm project status <project-id>          # Show overall status + burn-down
```

**DECIDED (Decision #5):** **BOTH HTTP API (on the existing tm daemon) AND MCP tools** (`mcp__trusty-mpm__project_*`) for agentic driving — same dual-surface pattern other trusty-* daemons already use.

**DECIDED (Decision #6):** Spec linking is **manual for MVP** — users explicitly set `spec_ref` when creating/updating a Deliverable. Auto-scan/heuristic matching deferred.

TUI (epic #1272) and Telegram surfaces will build on this foundation later.

### 4.2 Handoff from Project Manager to Session Manager

When a user invokes `tm project spawn-session <project-id>`:

1. **Project Manager** looks up the Project and Deliverable (if specified).
2. **Project Manager** calls Session Manager's `session_new(repo_url, ref, task)` with project context embedded (e.g., in the task string or a new metadata field).
3. **Session Manager** provisions the workspace and launches Claude Code (as before).
4. **Session Manager** returns a `SessionRecord` with `id`, `name`, `workspace_path`, etc.
5. **Project Manager** creates a `SessionBinding` linking the session to the Project/Deliverable.
6. **User interface** reflects the binding: `tm project show` lists active sessions; `tm session show` notes which Project it serves.

### 4.3 Session completion & Deliverable status updates

When a session completes (or is abandoned):

1. **Session Manager** marks the session as `dead` or `complete`.
2. **Project Manager** observes the state change and checks objective gates: tests green + trusty-review APPROVE/CI passing.
3. **If gates pass,** Project Manager auto-marks Deliverable complete; **if gates fail or unclear,** Project Manager prompts user to confirm completion status.
4. **User** confirms or rejects (via CLI, TUI, or Telegram if prompted).
5. **Project Manager** updates the Deliverable status and re-computes Project status.

**DECIDED (Decision #8):** Completion is **tiered by risk/gate outcome**. Auto-mark when session's work passed objective gates (tests green + trusty-review APPROVE/CI passing); otherwise prompt user to confirm. Matches the harness's existing "80% autonomous, escalate the ambiguous 20%" operating model.

**DECIDED (Decision #12):** Delivery verification gates on **same objective signals used to merge PRs** (tests green, trusty-review APPROVE/CI passing), surfaced to user for lightweight confirm — not blind trust in session self-report, not pure manual judgment either.

### 4.4 Project & Deliverable status transitions

**DECIDED (Decision #9):** Status transitions follow a **simple linear model with a blocked branch**:

```
Project/Deliverable lifecycle:
  proposed
    ↓
  in-progress
    ↙        ↘
  blocked  (active work)
    ↖        ↙
  in-progress
    ↓
  complete
    ↓
  delivered/shipped
```

**Transition rules:**
- `proposed → in-progress` — user starts work (spawn session)
- `in-progress → blocked` — user encounters blocker (manual trigger)
- `blocked → in-progress` — blocker resolved (manual trigger)
- `in-progress → complete` — auto-triggered by gate pass OR manual confirmation
- `complete → delivered/shipped` — user marks finished (ready for production/release)
- **No skipping:** cannot jump from `proposed` directly to `complete` or `delivered`
- **Milestone status:** mirrors rollup of contained Deliverables (proposed if all proposed, in-progress if any in-progress, etc.)

---

## 5. Non-Goals

**Explicitly OUT of scope for Project Manager (not Session Manager issues):**

1. **Does not write or edit code.** All coding happens inside sessions. Project Manager is the *project coordinator*, not the *engineer*.
2. **Does not replace Session Manager.** Session Manager's tmux/session mechanics are unchanged; Project Manager is a higher-level orchestrator.
3. **Does not manage individual spec authoring.** The user writes specs; Project Manager just *links* to them.
4. **Does not implement autonomy policy.** Autonomy decisions (T1–T4, auto-accept rules) remain Session Manager + AUTONOMY_POLICY.md concerns. Project Manager may *observe* autonomy signals but does not decide them.
5. **Does not coordinate cross-project dependencies.** A future phase (vNext) may handle "Feature X in Project A depends on Feature Y in Project B," but that is deferred.
6. **Does not generate estimates.** Estimation is a human activity (or future LLM-assisted); Project Manager just *tracks* estimates vs. actual.

---

## 6. Relationship to Adjacent Specs

### DOC-17 (Harness Runner Vision)
DOC-17 describes the north-star for autonomous session execution. Project Manager **consumes** that autonomy: it orchestrates sessions that, once spawned, run autonomously per DOC-17 principles.

### DOC-22 (Multi-Repo Routing)

**DECIDED (Decision #11, hierarchical model):** DOC-22's NL-to-repo resolver is an INTERNAL INPUT MECHANISM, not a competing entry point. Project Manager is the primary user proxy; DOC-22's resolver feeds it. A user says "build feature X" → DOC-22 resolves to Project → Project Manager orchestrates. This supersedes the current framing of DOC-22 and PM as "complementary"; they are now hierarchical.

### DOC-23 (Learned Autonomy)
DOC-23 describes learned decision auto-answering. Project Manager observes but does not drive this; Session Manager + AUTONOMY_POLICY own the gate. Project Manager may *expose* learned patterns (e.g., "Historical data: you approve 95% of style fixes") but does not implement the learning.

### Autonomy Tier Binding

**DECIDED (Decision #10):** Project Manager **inherits from Session Manager's existing AUTONOMY_POLICY.md T1-T4 tiers** — do NOT build a second parallel autonomy system. Project Manager can reference/display the tier but Session Manager remains the single source of truth. A Project may label which tier it operates under, but the actual gate logic is elsewhere.

### DOC-29 (Behavior Conformance)
DOC-29 enumerates testable harness behaviors (instruction assembly, self-awareness, memory integration, etc.). Project Manager, once implemented, will gain its own set of conformance rows (SPEC-PM-01 through SPEC-PM-XX) in a future update to DOC-29.

---

## 7. Resolved Design Decisions

These **12 design decisions** have been resolved by the owner and are now LOCKED for the MVP phase:

### Data model decisions

1. **Project ↔ Repo mapping (DECIDED):** Each Project is **1:1 with a git repo**. A monorepo is ONE Project; finer granularity is via Deliverables, not Projects.

2. **Estimation units (DECIDED):** Use **tiers (S/M/L/XL)**, not point-hours or ranges. Coarse-grained, consistent, avoids false precision.

3. **Deliverable breakdown (DECIDED):** Keep Deliverables **flat — no recursive sub-tasks**. Add hierarchy later if needed; MVP stays simple.

### User interaction decisions

4. **Primary surface for MVP (DECIDED):** **CLI first** (`tm project`), matching existing `tm` CLI patterns. TUI and Telegram come later.

5. **HTTP/MCP API exposure (DECIDED):** **BOTH HTTP (on existing tm daemon) AND MCP tools** (`mcp__trusty-mpm__project_*`) for agentic driving — same dual-surface pattern other trusty-* daemons use.

6. **Spec linking (DECIDED):** **Manual for MVP** — users explicitly set `spec_ref` when creating/updating Deliverables. Auto-scan/heuristics deferred.

### Semantics decisions

7. **Session ↔ Deliverable binding (DECIDED):** **1 Deliverable ↔ many Sessions** (not strict 1:1, not full N:M). Each session works on ONE deliverable at a time; a Deliverable accumulates multiple sessions over its life.

8. **Deliverable completion trigger (DECIDED):** **Tiered by risk/gate outcome**. Auto-mark when session's work passed objective gates (tests green + trusty-review APPROVE/CI passing); otherwise prompt user to confirm.

9. **Status transition model (DECIDED):** Simple linear + blocked branch: `proposed → in-progress → [blocked ↔ in-progress] → complete → delivered/shipped`. No skipping straight to complete/delivered from proposed.

### Integration decisions

10. **Autonomy tier binding (DECIDED):** **Inherit from Session Manager's AUTONOMY_POLICY.md T1-T4 tiers** — do NOT build a second autonomy system. PM references/displays the tier; SM remains single source of truth.

11. **Multi-repo coordination (DECIDED):** **Hierarchical — PM is primary surface**. DOC-22's NL→repo resolver is an INTERNAL INPUT MECHANISM: user intent (NL) → DOC-22 resolves to Project → PM orchestrates. SUPERSEDES prior "both are complementary entry points" framing.

12. **Delivery verification (DECIDED):** Gate on **same objective signals used to merge PRs** (tests green, trusty-review APPROVE/CI passing), surfaced to user for lightweight confirm — not blind self-report, not pure manual either. Matches the harness's "80% autonomous, escalate the ambiguous 20%" model.

---

## 8. Implementation Phases (Future Planning)

**This is rough guidance; Bob will refine during phase planning.**

### Phase 1: MVP — Project scaffold + Session binding
- `Project` CRUD API (create, list, get, update status)
- `Deliverable` CRUD API
- `SessionBinding` creation + listing
- CLI surface: `tm project {create, list, show, spawn-session}`
- HTTP API endpoints (extend `daemon/api.rs`)
- Disk-backed store (alongside existing `sessions.json`)

### Phase 2: Milestone + Estimation
- `Milestone` CRUD
- estimated_effort tier tracking + burn-down views (S/M/L/XL velocity)
- TUI dashboard integration (DOC-16 / #1272)
- Velocity calculation (moving average of actual vs. estimate)

### Phase 3: Spec linkage + Conformance tracking
- `SpecRef` linking (Project/Deliverable → docs/specs/file)
- Conformance matrix integration (DOC-29)
- Automated version tracking for linked specs

### Phase 4: Advanced features
- Autonomy tier configuration per Project
- Cross-project dependency tracking
- Telegram `/project` commands (DOC-19 integration)
- Web dashboard with timeline + burn-down charts

---

## 9. Related Work & Navigation

| Document | Relationship |
|----------|--------------|
| **DOC-17 (Harness Runner Vision)** | PM orchestrates autonomous sessions per this vision |
| **DOC-22 (Multi-Repo Routing)** | Hierarchical subordinate: DOC-22's NL→repo resolver is an internal input mechanism; PM is the primary user proxy (Decision #11) |
| **DOC-23 (Learned Autonomy)** | PM observes autonomy signals; Session Manager drives them |
| **DOC-29 (Behavior Conformance)** | PM will add SPEC-PM-01…XX rows in a future conformance update |
| **DOC-14 (Session Manager Agent)** | PM orchestrates Sessions via this contract |
| **SESSION_MANAGER_MVP.md** | PM builds atop Session Manager's HTTP API and session lifecycle |
| **DOC-16 (Interactive Sessions TUI, #1272)** | PM needs a dashboard view; TUI epic (#1272) will integrate |
| **AUTONOMY_POLICY.md** | PM observes tiers; does not decide them |

---

## 10. Appendix: Conformance Tracking Placeholder

When Project Manager is implemented, the conformance matrix (DOC-29) will be updated with entries like:

| Behavior | Status | Test |
|----------|--------|------|
| SPEC-PM-01: Project CRUD | *Not started* | `cargo test -p trusty-mpm --lib -- project_crud` |
| SPEC-PM-02: Deliverable lifecycle | *Not started* | `cargo test -p trusty-mpm --lib -- deliverable_lifecycle` |
| SPEC-PM-03: Session-Project binding | *Not started* | `cargo test -p trusty-mpm --lib -- session_binding` |
| … | … | … |

This placeholder reminds future implementers that Project Manager behaviors must eventually be added to DOC-29's testable conformance table.

---

## 11. Changelog

**v2 (2026-07-01, all 12 design questions resolved)**
- Resolved all 12 open design questions from v1 per owner decisions.
- Section 7 rewritten: "Open Questions" → "Resolved Design Decisions" with 1-line rationales for each.
- Updated Section 2.3: DOC-22 relationship is now HIERARCHICAL (PM is primary, DOC-22 resolver is internal input).
- Updated Section 3: Deliverable model uses tier-based estimation (S/M/L/XL), flat (no recursion).
- Updated Section 4: CLI-first for MVP, dual HTTP+MCP API, manual spec linking, tiered completion gates.
- Added Section 4.4: Status transition state machine (linear + blocked branch, no skipping).
- Updated Section 6: Autonomy tier binding inherits from Session Manager; DOC-22 is hierarchical subordinate.
- Status remains DRAFT/Vision (0% implemented); no implementation phase changes, just design locked.

**v1 (2026-07-01, initial draft)**
- Established Project Manager as a NEW layer above Session Manager, not a rename/replacement.
- Defined core concepts: Project, Deliverable, Milestone, SpecRef, SessionBinding.
- Articulated the architectural boundary: PM owns project lifecycle + user surface; SM owns session/tmux mechanics.
- Listed 12 open questions for phase planning.
- Noted that implementation is deferred; this is DESIGN ONLY.

