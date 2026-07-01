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

DOC-22 specifies the **project registry** and **NL-to-repo resolver** (mapping `"my-app"` or a ticket ID to a repo). The routing layer is **dual-owned**:
- **Session Manager perspective:** the routing layer is a seam that maps NL intent to `(project_name, repo_url, ref)` inputs for session creation.
- **Project Manager perspective:** the routing layer is *one* way a user surfaces intent; Project Manager provides the *explicit* project definition interface above routing.

In a future implementation, Project Manager would likely **consume** the routing layer's resolver to support both implicit (NL) and explicit (project-selected) session spawning.

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

**Open questions:**
- Should a Project be 1:1 with a git repo, or can one Project span multiple repos (e.g., a monorepo with sub-projects)?
- Who can create a Project — just the owner, or any team member? Should there be a "project template" for recurring patterns (bug fix, feature, refactor)?

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
  estimated_effort_hours: Option<f64>,
  actual_effort_hours: Option<f64>,
  created_at: DateTime,
  target_date: Option<DateTime>,
  assigned_to: Option<String> (which harness/agent is working on this?),
}
```

**Open questions:**
- Is estimated_effort_hours a point estimate, range, or tier (S/M/L/XL)? Should it integrate with historical velocity data?
- Should Deliverables be sub-breakdownable (a Feature has sub-tasks)?
- How is the user prompted to create Deliverables — manually, auto-imported from GitHub issues, or inferred from specs?

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

**Open question:** Should a single session work on multiple Deliverables, or is it 1:1? Should a Deliverable be worked on by multiple concurrent sessions?

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

**Option A: CLI (`tm project` namespace)**
```
tm project list                         # List all projects
tm project create --name "feature-x" --repo <url> --due-date <date>
tm project show <project-id>            # Show project + deliverables + milestones + sessions
tm project add-deliverable <project-id> --name "..." --type feature --estimate 8h
tm project add-milestone <project-id> --name "v1.0" --target-date <date>
tm project update <project-id> --status in-progress  # or delivered, shipped, etc.
tm project spawn-session <project-id> --deliverable <id> --task "..."
tm project status <project-id>          # Show overall status + burn-down
```

**Option B: TUI dashboard (ties to epic #1272)**
- Left sidebar: list of Projects (grouped by status)
- Center: selected Project detail (Deliverables, Milestones, Sessions, Notes)
- Right pane: active Session pane output (if a session is running)
- Key actions: Create Project/Deliverable/Milestone, spawn Session, update status

**Option C: Telegram / Web surface**
- Align with DOC-19 (TELUI) patterns
- `/project` commands mirror CLI surface
- Web dashboard for long-form project view (burn-down charts, timeline, handoff tracking)

**Open questions:**
- Which surface is primary for MVP? (CLI alone, TUI, both?)
- Should Project Manager expose an HTTP API (extending `daemon/api.rs`), an MCP interface, or both?
- How do users link Projects to Specs? Manually via UI, auto-scanned from directory, or both?

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
2. **Project Manager** observes the state change and prompts: "Was this session for Deliverable X? Mark it as complete?"
3. **User** confirms (via CLI, TUI, or Telegram).
4. **Project Manager** updates the Deliverable status and re-computes Project status.

**Open question:** Should this be automatic (session completion → auto-mark deliverable), manual (user confirms), or tiered (automatic for low-risk changes, manual for high-risk)?

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
DOC-22 specifies NL-to-repo resolution and project registry. Project Manager **provides an explicit alternative** to NL routing: a user can explicitly select or create a Project, whereas DOC-22's routing is more implicit/fuzzy. Both are valid entry points; the implementations should coexist.

### DOC-23 (Learned Autonomy)
DOC-23 describes learned decision auto-answering. Project Manager observes but does not drive this; Session Manager + AUTONOMY_POLICY own the gate. Project Manager may *expose* learned patterns (e.g., "Historical data: you approve 95% of style fixes") but does not implement the learning.

### DOC-29 (Behavior Conformance)
DOC-29 enumerates testable harness behaviors (instruction assembly, self-awareness, memory integration, etc.). Project Manager, once implemented, will gain its own set of conformance rows (SPEC-PM-01 through SPEC-PM-XX) in a future update to DOC-29.

---

## 7. Open Questions for Next Phase

These are **design decisions Bob and the team must make**:

### Data model
1. **Project ↔ Repo mapping:** Is each Project 1:1 with a git repo, or can Projects span multiple repos (e.g., monorepo with sub-projects)?
2. **Estimation units:** Should `estimated_effort_hours` be a point estimate, a range (min/max), or a tier (S/M/L/XL)? Should it integrate with historical velocity?
3. **Deliverable breakdown:** Can Deliverables be recursively sub-divided (Feature → Sub-tasks), or are they flat?

### User interaction
4. **Primary surface for MVP:** CLI (`tm project`), TUI (dashboard), or both?
5. **HTTP/MCP API:** Should Project Manager expose an HTTP API (extending `daemon/api.rs`), MCP tools, or both? Does Telegram routing (DOC-19 / DOC-22) drive the surface choice?
6. **Spec linking:** Should users manually link Specs, auto-scan from `docs/specs/` + heuristics, or both?

### Semantics
7. **Session ↔ Deliverable binding:** 1:1 (each session works on one Deliverable) or N:M (multiple sessions on one Deliverable)?
8. **Deliverable completion:** Automatic on session completion, manual confirmation, or tiered by risk level?
9. **Status transitions:** What state machine governs Project/Deliverable/Milestone transitions? Are some transitions forbidden?

### Integration
10. **Autonomy policy binding:** Does Project Manager get a project-level autonomy tier (e.g., "this Project runs at T2 auto-accept"), or does it inherit from Session Manager's existing tier model?
11. **Multi-repo coordination:** How do Projects interact with DOC-22's multi-repo routing? Are they redundant, complementary, or hierarchical?
12. **Handoff protocol:** When a session completes, who decides if a Deliverable is done — the user (manual), the session's own completion signal, or heuristics?

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
- `estimated_effort_hours` tracking + burn-down views
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
| **DOC-22 (Multi-Repo Routing)** | Complementary: routing is NL-implicit; PM is explicit |
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

**v1 (2026-07-01, initial draft)**
- Established Project Manager as a NEW layer above Session Manager, not a rename/replacement.
- Defined core concepts: Project, Deliverable, Milestone, SpecRef, SessionBinding.
- Articulated the architectural boundary: PM owns project lifecycle + user surface; SM owns session/tmux mechanics.
- Listed 12 open questions for phase planning.
- Noted that implementation is deferred; this is DESIGN ONLY.

