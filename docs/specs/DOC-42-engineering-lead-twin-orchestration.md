---
spec_refs:
  - id: SPEC-TWIN-01~draft
    path: docs/specs/DOC-42-engineering-lead-twin-orchestration.md
    anchor: SPEC-TWIN-01~draft
  - id: SPEC-TWIN-02~draft
    path: docs/specs/DOC-42-engineering-lead-twin-orchestration.md
    anchor: SPEC-TWIN-02~draft
  - id: SPEC-TWIN-03~draft
    path: docs/specs/DOC-42-engineering-lead-twin-orchestration.md
    anchor: SPEC-TWIN-03~draft
  - id: SPEC-TWIN-04~draft
    path: docs/specs/DOC-42-engineering-lead-twin-orchestration.md
    anchor: SPEC-TWIN-04~draft
---

# DOC-42 — Engineering Lead / Virtual Twin Cross-Tool Orchestration Architecture

**Status:** DRAFT — Vision & Design Work Only (0% implemented)
**Subsystem:** trusty-agents — multi-workstream lead orchestration / cross-tool supervision
**Owner:** Engineering (trusty-agents, trusty-mpm, trusty-code)
**Last-updated:** 2026-07-18
**Spec ID:** `SPEC-TWIN-01~draft` … `SPEC-TWIN-04~draft` (DOC-42)
**Epic:** #2109 — `tm manager`: Layer-3 Chat-Based Portfolio Project Manager (cross-tool unification, refs DOC-36)
**Builds on:**
- DOC-36 ([`tm manager`: Layer-3 Chat-Based Portfolio Project Manager](./tm-manager-vision.md)) — the PM-of-PMs concept, decision authority, escalation to user.
- DOC-35 ([`tm project`: Deterministic Project/Session Control Plane](./tm-project-control-plane.md)) — project/session lifecycle, deterministic control surface for tm (trusty-mpm).
- DOC-39 ([trusty-code Harness UI](./trusty-code-harness-ui.md)) — session management for tcode (trusty-code), JSON-RPC + REST API surface.
- DOC-40 ([Durable Background Agents](./durable-background-agents.md)) — exclusive attach/detach semantics, durable agent lifecycle.
- DOC-38 ([Spec-Linked Documentation](./spec-linked-documentation.md)) — reference standard for this spec.
**Cross-ref:** issue #2109 (layer-3 project manager, `tm manager`), issue #2983 (tcode REST slice 2), `crates/trusty-agents/src/`, `crates/trusty-mpm/src/client/proxy.rs`, `crates/trusty-code/src/session/protocol.rs`

> **Scope note.** This is a **vision and architectural boundary spec**. It describes a
> unified cross-tool orchestration layer — the "engineering lead" or "virtual twin" agent —
> that supervises multiple workstreams spanning both `tm` (trusty-mpm, CLI/TUI single-workstream)
> and `tcode` (trusty-code, Tauri GUI multi-workstream) environments. The spec defines:
> (1) **Three separated concerns**: deterministic connectors (tool control), the persistent
> lead agent (decision + multi-workstream supervision), and per-workstream liaisons
> (continuous verbose supervision); (2) **Confidence-gated authority**: the lead acts
> autonomously only when confident an action matches user intent, reverting to user
> on uncertainty; (3) **No workstream migration**: tool assignment is done at ticket level
> by the lead (or user), and workstreams remain in their assigned tool throughout their
> lifecycle. **This spec is DESIGN-ONLY** — no code changes are included. Phased
> implementation roadmap is deferred to issue-driven PR planning.

---

## 1. Motivation & Problem {#SPEC-TWIN-01~draft}

### 1.1 The status quo: Tool-siloed workstreams

Today, trusty-mpm (`tm`) and trusty-code (`tcode`) operate as **independent harnesses**:
- `tm` is single-workstream: one Claude Code session per `tm` instance; supervision is per-session (activity monitor, REPL control).
- `tcode` is multi-workstream: multiple Sessions can run concurrently; the Tauri GUI is the coordination surface.
- **No cross-tool orchestration:** a user managing multiple workstreams across both tools has no unified "project manager" view. Each tool has its own session control and decision surface; the user must manually correlate priorities and handoff decisions.
- **No persistent decision layer:** decisions about "which workstreams matter most," "which one should block on blockers," "when to escalate," and "what are this user's standards (SLOS, test coverage, review time)?" are made ad-hoc, not persisted or propagated.

### 1.2 The vision: A single persistent "engineering lead" agent

Bob's directive (#2109, #2036): **The user's workstreams should be supervised by a persistent "engineering lead" or "virtual twin"** — a durable agent running in `trusty-agents` that:
1. **Holds intent + standards:** knows the user's stated goals, delivery commitments, quality bar, risk tolerance, and escalation thresholds. (Backed by trusty-memory.)
2. **Coordinates multiple workstreams:** supervises both `tm` and `tcode` sessions in parallel, understanding which workstreams are progressing and which are blocked or drifting.
3. **Makes confident decisions autonomously:** when the lead is confident an action matches the user's intent (within learned/calibrated confidence bounds), it acts without asking. When uncertain, it escalates to the user.
4. **Never moves workstreams between tools:** tool assignment is a ticket-level decision; once a workstream is assigned to `tm` or `tcode`, it stays there for its lifetime.
5. **Routes work via connectors, not duplicating logic:** both `tm` and `tcode` expose deterministic control surfaces (SessionManager API for tm, REST/JSON-RPC for tcode); the lead uses a unified `WorkstreamConnector` trait to abstract both.

### 1.3 Why now?

- **tm is mature:** Session control, activity monitoring, and REPL I/O are solid (crates/trusty-mpm/src/client/proxy.rs, daemon/mcp_session.rs, 13 ops).
- **tcode is nearing control parity:** REST session routes (S2, #2983) are shipping; daemon lifecycle, auth, and control endpoints are nearly ready (DOC-39, prerequisite items).
- **Agents & memory are ready:** trusty-agents framework exists; trusty-memory MCP is durable and accessible. No new infrastructure blocker.
- **The gap is coordination:** there is no "PM-of-PMs" that can decide and route across both tools. DOC-36 (`tm manager`) is the decision layer; DOC-42 (this spec) defines how it supervises heterogeneous workstreams.

---

## 2. Three-Layer Model {#SPEC-TWIN-02~draft}

The architecture cleanly separates three concerns:

### 2.1 Layer 1: Connectors = Deterministic Tool Control (NOT agents)

A **Connector** is a thin, **stateless tool** that translates unified control commands into tool-specific I/O. It is **NOT an agent**; it has no memory, no decision-making, no supervision. It is callable by the lead agent, routes work, and returns structured status.

**Characteristics:**
- Deterministic: same input → same output (idempotent control ops).
- Tool-specific implementation: tm connector uses SessionManager API (HTTP + MCP), tcode connector uses REST + JSON-RPC.
- Unified interface: both implement `WorkstreamConnector` trait (home: trusty-agents-common).
- Control operations: create/list/status/send/attach/detach/delegate (all return structured JSON or Result).
- **Stateless:** no session registry, no memory of prior workstreams. Lead agent owns that.

**Two implementations (mature + nascent):**

1. **tm Connector (crates/trusty-mpm/src/client/proxy.rs + daemon/mcp_session.rs)**
   - Mature: 13 ops, HTTP + MCP, SessionManager auth, session registry.
   - Already used by tm CLI, session TUI, Telegram bot.
   - Exposes: `create_session`, `list_sessions`, `session_status`, `send_input`, `attach`, `detach`, `delegate_agent`, …

2. **tcode Connector (crates/trusty-code/src/session/protocol.rs + serve/http.rs)**
   - Nascent: JSON-RPC session protocol + REST routes (DOC-39 S2).
   - Prerequisite: REST auth wiring (control-endpoint auth), daemon lifecycle management, `delegate` endpoint.
   - Exposes: `create_session`, `list_sessions`, `session_status`, `send_input`, `attach`, `detach`, `delegate`, …

**Why separate connectors from the lead?**
- **Avoids duplication:** lead logic is tool-agnostic; connector logic is concentrated in tool-specific crates.
- **Keeps the lead thin:** the lead calls a trait method; each tool's connector owns its own API surface.
- **Enables testing:** connectors can be unit-tested independently; the lead can be tested with mock connectors.
- **Preserves tool autonomy:** tm and tcode keep their own session registries and control flows; the lead is a supervisor, not a master.

### 2.2 Layer 2: The Lead (Persistent Virtual Twin)

The **Lead** is a **single persistent agent** (running in trusty-agents) that:

1. **Holds state:**
   - Current intent + user-stated goals (from trusty-memory).
   - Standards & SLOs: code quality bar, test coverage, review time, escalation thresholds.
   - Workstream ledger: which workstreams are active, their tool assignment, their status, their dependencies.
   - Confidence calibration: learned thresholds for "I am confident enough to act autonomously."
   - Audit trail: durable log of decisions, escalations, and outcomes.

2. **Supervises workstreams:**
   - Polls both `tm` and `tcode` via their connectors (parallel or fan-out).
   - Understands workstream status (blocked, in-progress, complete).
   - Makes routing decisions: "which workstream should I prioritize next?"
   - Spawns liaisons for continuous supervision (see Layer 3).

3. **Makes confident decisions:**
   - Decides to create a new session, assign it to a tool, start work on a ticket.
   - Decides to send code review feedback, request debug output, suggest a refactor.
   - Decides to escalate (user override, new constraint, priority conflict).
   - All decisions are **confidence-gated**: only acts autonomously if `confidence >= threshold`.

4. **Escalates on uncertainty:**
   - When confidence is low: "I'm not sure which workstream to unblock next," or "I don't know your risk tolerance here," the lead **asks the user** rather than guessing.
   - Escalation is durable: recorded in audit trail so the user's answer refines future confidence.

5. **Does NOT:**
   - Write or execute code directly (work happens in sessions, via liaisons).
   - Stream raw session I/O into its context (that is the liaison's job).
   - Duplicate connector logic (routing is done via connectors).
   - Own session lifecycle details (create/manage tmux panes, etc. — connectors do).

**Home:** trusty-agents-common (or a new trusty-agents-lead crate if it grows large).

### 2.3 Layer 3: Per-Workstream Liaisons (Verbose Supervision)

A **Liaison** is a short-lived **continuous supervisor agent**, spawned per workstream by the lead. Each liaison:

1. **Watches one workstream:** monitors its session, ingests I/O, understands progress.
2. **Streams to the lead (summaries only):** periodically sends **structured summaries** (progress, blockers, next-step suggestions) to the lead; does NOT dump raw session output.
3. **Acts on lead directives:** when the lead says "send this code review," "escalate this blocker," "wrap up," the liaison implements it.
4. **Lives until workstream ends:** liaison lifecycle mirrors workstream lifecycle.
5. **Is tool-specific:** a tm liaison talks to the tm connector; a tcode liaison to the tcode connector. (But they are similar enough that one Liaison generic trait can serve both.)

**Why liaisons?**
- **Keep lead context small:** the lead sees summaries, not raw I/O.
- **Enable concurrent supervision:** multiple liaisons watch multiple workstreams in parallel.
- **Reduce token cost:** each liaison's context is bounded; the lead's is not unbounded by number of sessions.
- **Handle tool-specific I/O idioms:** tm liaison understands REPL commands, pane output; tcode liaison understands JSON-RPC events. Lead doesn't care.

**Liaison states:**
- Spawned → Watching → (Summary → Watch) loop → Wrap-up → Ended.

---

## 3. Confidence-Gated Authority {#SPEC-TWIN-03~draft}

Authority is **NOT automatic**. The lead acts autonomously only when **confident** an action aligns with user intent.

### 3.1 Design principle: Calibrated autonomy

The lead **knows it doesn't know everything.** It is trained/calibrated to:
- Act confidently (without asking) on **routine, low-risk decisions** ("I'll attach the debugger because the user always wants full context").
- **Ask first** on **high-stakes decisions** ("Should we break this feature into a separate ticket? I'm only 45% confident.").
- **Track accuracy over time:** when the user overrides a decision, the lead updates its confidence model.

### 3.2 Confidence signal & decision gate

Every decision the lead makes carries a **confidence score** (0–1 or Low/Mid/High). The lead compares it against a **decision threshold**:

- **High-confidence decisions (≥ `threshold`):** execute autonomously, log to audit trail.
- **Low-confidence decisions (< `threshold`):** escalate to user, record user's answer, update confidence model.

**Confidence sources:**
- **User history:** "The user has always approved refactors of > 200 SLOC. This is 180 SLOC. Confidence: 0.8."
- **Explicit intent:** "The user said 'ship this test suite by Friday.' Today is Wednesday. Confidence: 0.9."
- **Learned defaults:** "The user hasn't specified code-review SLO. Using learned prior. Confidence: 0.5."
- **Conflict / ambiguity:** "Two blockers, one dev-env issue, one upstream delay. User didn't rank them. Confidence: 0.3."

### 3.3 Confidence-gated states: Analogy to circuit breaker

The lead uses a **state machine** inspired by `crates/trusty-agents/src/circuit.rs` (Closed/Open/HalfOpen):

- **Closed (Confident mode):** acting autonomously on every decision at or above `threshold`.
- **Open (Safe mode):** user has given feedback contradicting lead's assumptions. Lead is conservative: asks first on most decisions, updates confidence model. (Analogous to circuit breaker's "open" = stop trusting the happy path.)
- **HalfOpen (Calibrating mode):** user provided a clarification or standard. Lead tries one decision with renewed confidence, then re-evaluates. (Analogous to circuit breaker's "half-open" = probe to see if conditions improve.)

**Trigger transitions:**
- **Closed → Open:** user overrides a high-confidence decision (accuracy mismatch).
- **Open → HalfOpen:** user provides explicit intent/SLO ("Always break features >200 SLOC").
- **HalfOpen → Closed:** probe decision succeeds (matches user's feedback).
- **HalfOpen → Open:** probe decision fails (user overrides again).

### 3.4 Intent representation (durable)

The lead stores **explicit intent** in trusty-memory:

```
{
  "user_goals": ["ship feature X by date Y", "maintain 85% test coverage"],
  "slos": {
    "code_review_time_hours": 4,
    "test_coverage_min": 0.85,
    "breaking_change_escalation": true
  },
  "risk_tolerance": "moderate",
  "escalation_thresholds": {
    "budget_overrun_percent": 10,
    "deadline_slip_days": 2,
    "blocker_count": 3
  },
  "learned_patterns": [
    { "pattern": "refactor > 200 SLOC", "user_decision": "break_into_separate_ticket", "confidence": 0.85 }
  ]
}
```

Accessible via `trusty-memory` MCP, updated by:
- User commands (`tm lead set-slo code-review-time 4h`).
- Escalation outcomes (when user overrides, learn from it).
- Explicit API (lead updates memory after each user interaction).

### 3.5 Escalation & audit trail

When the lead decides to escalate (confidence too low):

1. **Formulate escalation:** describe the decision, the context, the confidence score, and the options.
2. **Send to user** (via trusty-memory, or an escalation endpoint).
3. **Wait for user input** (or timeout to a safe default).
4. **Record outcome:** user's response → update confidence model + intent.
5. **Continue:** lead re-evaluates the decision with updated intent.

**Audit trail** (durable, in trusty-memory or a sidecar log):

```
{
  "timestamp": "2026-07-18T14:32:00Z",
  "decision_id": "uuid",
  "decision_type": "create_session | send_code_review | escalate_blocker | ...",
  "context": { "workstream_id": "...", "ticket": "#2109" },
  "confidence_before": 0.45,
  "action_taken": "escalated",
  "user_response": "create session for bug-fix",
  "confidence_after": 0.75,
  "notes": "user clarified priority; tcode > tm for this ticket"
}
```

---

## 4. Workstream Ledger & Tool Assignment {#SPEC-TWIN-04~draft}

### 4.1 Design principle: No mid-life migration

**DECIDED:** A workstream is **assigned to a tool at creation** (by the lead or user) and **remains in that tool** for its entire lifecycle. The tool assignment is done at the **ticket level**, not the session level.

**Why?**
- **Simplifies routing:** lead doesn't need to handle "move this workstream from tm to tcode mid-sprint."
- **Preserves tool-specific state:** tm's REPL session, tcode's interactive editor — these don't transfer cleanly.
- **Clearer responsibility:** "this ticket is being worked on in tm" is unambiguous.

### 4.2 Workstream ledger

The lead maintains a **workstream registry** (trusty-memory-backed, or a heterogeneous ledger in trusty-agents-common):

```
Workstream {
  id: String (UUID),
  ticket_ref: String (e.g., "#2109"),
  title: String,
  assigned_tool: ToolId (tm | tcode),    // IMMUTABLE after creation
  session_ids: Vec<SessionId>,           // one or more sessions in the assigned tool
  status: WorkstreamStatus (
    proposed | in_progress | blocked | complete | delivered
  ),
  created_at: DateTime,
  target_date: Option<DateTime>,
  priority: Priority (P0 | P1 | P2 | P3),
  blockers: Vec<BlockerRef>,
  dependencies: Vec<WorkstreamId>,
  lead_confidence: f64,                  // current confidence score
  state: ConfidenceState (Closed | Open | HalfOpen),
  last_updated: DateTime,
}
```

**Tool-specific session tracking:**
- For `tm` workstreams: `session_ids` are managed by tm connector (one active session per workstream).
- For `tcode` workstreams: `session_ids` can be multiple (concurrent sessions within tcode).

### 4.3 Creation: Who decides tool assignment?

Two paths:

1. **Lead-driven (autonomous):** lead decides based on:
   - Ticket type (small bug fix → tm; interactive UI work → tcode).
   - User history (learned preference).
   - Workload (which tool is less busy?).
   - Confidence threshold: if ≥ threshold, assign autonomously; else ask user.

2. **User-driven (explicit):** user specifies `--tool tm` or `--tool tcode` when creating a ticket/workstream.

**Lead respects user's explicit choice** (high confidence: 0.95+).

---

## 5. Implementation Domains & Crate Homes

### 5.1 New code: Where does it live?

| Component | Home Crate | Files / Modules |
|-----------|-----------|---|
| **WorkstreamConnector trait** | trusty-agents-common | `src/connectors/mod.rs`, `src/connectors/trait.rs` |
| **tm Connector** | trusty-mpm | `src/client/proxy.rs` (extend existing) or `src/connectors/tm.rs` |
| **tcode Connector** | trusty-code | `src/session/connector.rs` (new) |
| **Workstream Ledger** | trusty-agents-common | `src/workstream/ledger.rs` |
| **Confidence Gate & State Machine** | trusty-agents-common | `src/confidence/gate.rs`, `src/confidence/state.rs` |
| **Lead Agent** | trusty-agents or trusty-agents-common | `src/agents/lead.rs` or new `trusty-agents-lead/` crate |
| **Liaison Agent** | trusty-agents or trusty-agents-common | `src/agents/liaison.rs` |
| **Audit Log** | trusty-common or trusty-agents-common | `src/audit/trail.rs` |

### 5.2 Connector trait sketch

Unified interface, both connectors implement:

```rust
/// WorkstreamConnector: tool-agnostic session control surface.
/// Implementations: tm (via SessionManager), tcode (via REST + JSON-RPC).
pub trait WorkstreamConnector: Send + Sync {
    async fn create_session(
        &self,
        req: CreateSessionReq,
    ) -> Result<SessionInfo>;

    async fn list_sessions(&self) -> Result<Vec<SessionInfo>>;

    async fn session_status(&self, session_id: &str) -> Result<SessionStatus>;

    async fn send_input(&self, session_id: &str, input: &str) -> Result<()>;

    async fn attach(&self, session_id: &str) -> Result<AttachHandle>;

    async fn detach(&self, attach_id: &str) -> Result<()>;

    /// Delegate work: spawn a sub-agent within the session.
    async fn delegate(
        &self,
        session_id: &str,
        agent_spec: &AgentSpec,
    ) -> Result<DelegateHandle>;

    async fn workstream_status(&self, workstream_id: &str) -> Result<WorkstreamStatus>;
}
```

### 5.3 Liaison trait sketch

```rust
pub trait Liaison: Send + Sync {
    /// Spawn a liaison attached to a workstream.
    async fn spawn(
        workstream: &Workstream,
        connector: Arc<dyn WorkstreamConnector>,
        lead_tx: mpsc::Sender<LiaisonMessage>,
    ) -> Result<Self>;

    /// Watch the workstream; emit periodic status summaries to lead.
    async fn watch(&mut self) -> Result<()>;

    /// Accept a directive from the lead.
    async fn execute_directive(&mut self, directive: &Directive) -> Result<()>;

    /// Wrap up: finalize workstream, emit completion summary.
    async fn wrap_up(&mut self) -> Result<WrappedUpSummary>;
}
```

---

## 6. Non-Goals

**This spec does NOT include:**

- **UI.** No tm-specific GUI changes. (tcode already has a Tauri GUI; tm remains CLI/TUI/REPL. GUI is a separate decision.)
- **Workstream migration.** No moving sessions between tools mid-lifecycle.
- **Separate orchestration database.** Lead state lives in trusty-memory; workstream ledger is append-only and recreatable from audit trail.
- **tcode operational prerequisites.** This spec assumes tcode has REST auth, daemon lifecycle, and control-endpoint auth wired (DOC-39 prerequisites, #2983 S3+); those are out of scope here but are hard blockers for implementation.
- **Confidence model training.** The confidence gate is a state machine (Closed/Open/HalfOpen) plus user feedback. Building a statistical ML model is future work.
- **Multi-user scenarios.** This spec assumes single-user (the "engineering lead" is one user's twin). Multi-user orchestration is a separate problem.

---

## 7. Explicit Prerequisites (Blockers for Implementation)

These items must be DONE before lead agent work can start:

### 7.1 tcode parity items (needed for tcode Connector to be viable)

From DOC-39 and issue #2983:

- [ ] REST session routes implemented and tested (DOC-39, S2 merged).
- [ ] REST control-endpoint **auth wiring** (`Authorization` header, identity verification).
- [ ] `POST /sessions/:id/delegate` endpoint (parallel to tm's `delegate_agent` MCP op).
- [ ] Daemon lifecycle management: graceful shutdown, recovery, SIGTERM handling.
- [ ] Session registry audit trail (so lead can observe session history across restarts).

### 7.2 trusty-memory enhancements (needed for intent + audit trail)

- [ ] Structured schema for "user intent" (goals, SLOs, risk tolerance).
- [ ] Audit log schema (decision trail, confidence scores, escalations).
- [ ] Query API: fetch intent, fetch audit trail, append audit record.

### 7.3 trusty-agents-common enrichments

- [ ] Trait and module structure for connectors.
- [ ] Workstream ledger and registration.
- [ ] Confidence gate and state machine (ported from circuit.rs).

---

## 8. Phased Implementation Roadmap

Below is a phased delivery plan, with effort estimates (S=Small, M=Medium, L=Large).

### Phase 1: Connectors & Unified Control Surface [M]

**Goal:** Both tm and tcode expose equivalent, trait-based session control.

**Work:**
1. Define `WorkstreamConnector` trait in trusty-agents-common. [S]
2. Implement tm Connector (wrap existing SessionManager API). [S]
3. Implement tcode Connector (wire REST + JSON-RPC). [M]
4. Integration test both connectors (mock + real sessions). [M]

**Deliverables:** both connectors, trait passing integration tests, trait documented.

**Prerequisite:** tcode REST parity (DOC-39 S2, #2983).

### Phase 2: Workstream Ledger & Heterogeneous Registry [S]

**Goal:** Lead can track workstreams across both tools in a unified ledger.

**Work:**
1. Define `Workstream` struct and registry interface. [S]
2. Back registry with trusty-memory (or append-only log). [S]
3. Implement create/list/update/query API. [S]
4. Test registry recovery (load from trusty-memory after restart). [S]

**Deliverables:** workstream registry, API, tests.

**Prerequisite:** Phase 1 complete.

### Phase 3: Durable Audit Log [M]

**Goal:** Every decision, escalation, and user response is logged durably and queryable.

**Work:**
1. Design audit schema (decision_type, confidence_before/after, user_response, timestamp). [S]
2. Implement audit trail writer (append to trusty-memory or sidecar log). [S]
3. Implement audit trail reader/query (filter by workstream, date range, decision type). [M]
4. Integrate with lead agent (every decision logs; every escalation logs user response). [M]

**Deliverables:** audit trail, schema, reader, writer, integration.

**Prerequisite:** Phase 1 + 2.

### Phase 4: Confidence Gate & State Machine [M/L]

**Goal:** Lead can make autonomous decisions (Closed state), detect misalignment (Open), and refine (HalfOpen).

**Work:**
1. Port `circuit.rs` Closed/Open/HalfOpen model to a `ConfidenceGate` struct. [S]
2. Implement confidence scoring logic (history-based, intent-based, default priors). [M]
3. Implement decision gate: compare confidence vs. threshold, emit escalation if needed. [M]
4. Integrate with trusty-memory: store/fetch intent, update confidence model on escalation outcome. [M]
5. Integration test: lead makes a few decisions, user overrides one, confidence model updates, next similar decision escalates. [L]

**Deliverables:** confidence gate, state machine, decision gate, integration.

**Prerequisite:** Phase 2 + 3.

### Phase 5: Lead Agent Core [L]

**Goal:** A persistent agent in trusty-agents that holds workstreams, supervises via connectors, spawns liaisons.

**Work:**
1. Implement Lead agent (spawn, event loop, connector calls, liaison spawn). [L]
2. Implement Liaison agent (watch, summary emission, directive execution). [M]
3. Wire lead to connectors (create/list/status calls). [M]
4. Implement liaison lifecycle (spawn on workstream creation, wrap up on completion). [M]
5. End-to-end test: lead creates two workstreams (one tm, one tcode), spins up liaisons, receives summaries. [L]

**Deliverables:** lead + liaison agents, integration with connectors + workstream ledger.

**Prerequisite:** Phase 1–4.

### Phase 6: tcode REST + Auth Parity (if not done in Phase 1) [S/M]

**Goal:** tcode REST and auth are complete so tcode Connector can be shipped.

**Work:**
1. Complete REST route wiring (session CRUD, delegate endpoint). [S]
2. Implement control-endpoint auth (`Authorization` header, identity check). [M]
3. Integration test (helm/local daemon + HTTP client). [S]

**Deliverables:** tcode REST complete, auth working, integration tests passing.

**Prerequisite:** DOC-39 S2 merged.

**Note:** Can run in parallel with Phases 1–4 if needed.

---

## 9. Open Questions & Uncertainties

### Q1: Where does the Lead crate live?

**Option A:** `trusty-agents-common` (smaller, shared): Lead is a thin agent template; each deployment instantiates it.

**Option B:** `trusty-agents` (or new `trusty-agents-lead`): Lead is a bundled, domain-specific agent; users load it via agent registry.

**Decision deferred:** depends on how many shared abstractions are needed. Recommend starting in trusty-agents-common, move to own crate if >300 SLOC.

### Q2: Confidence signal source — learned or hardcoded?

**Option A:** Hardcoded priors (e.g., "refactors >200 SLOC always break into separate ticket" = confidence 0.9).

**Option B:** Learned from user feedback (user overrides a decision → lower confidence next time).

**Option C:** Hybrid (priors + learning).

**Decision:** Start with **hybrid** (hardcoded priors + user feedback updates). ML model is future work.

### Q3: Single lead agent or per-project lead?

**Option A:** Single lead for all workstreams across both tools.

**Option B:** One lead per project (tm project = lead; tcode project = separate lead).

**Decision:** Start with **single lead** (simpler, matches DOC-36's "PM-of-PMs"). Per-project leads can be added if scaling requires it.

### Q4: Liaison supervision frequency & verbosity?

**How often should liaisons poll and emit summaries?** (Every session output? Every 30s? On state change?)

**Decision deferred:** start with **state-change driven** (summarize when workstream status changes). Tune frequency based on early feedback.

### Q5: Escalation channel — how does the user interact?

**Option A:** MCP endpoint (trusty-memory escalation queue).

**Option B:** Telegram bot (same as `tm` already uses).

**Option C:** REST endpoint in lead daemon.

**Option D:** Hybrid (all of the above).

**Decision deferred:** start with **trusty-memory** (already durable + queryable). Add Telegram/REST as UI layer later.

---

## 10. Success Criteria

### Minimal Viable Implementation

- [ ] Both connectors (tm + tcode) successfully create, list, status sessions.
- [ ] Lead agent spawns, polls both connectors, emits workstream status.
- [ ] Liaison agent watches one session and emits summaries.
- [ ] One escalation completes (low-confidence decision → user answer → confidence model updated).
- [ ] Audit trail records all decisions and outcomes.
- [ ] Integration test: lead + liaison + both connectors work end-to-end.

### Beyond MVP

- [ ] Lead agent runs persistently (survives session restarts).
- [ ] Workstream ledger survives lead restarts.
- [ ] Confidence model is tuned (accuracy > 80% on 50 decisions).
- [ ] Telegram / REST escalation channels wired.
- [ ] DOC-36 (`tm manager`) integration complete.

---

## 11. References & Related Specs

- **DOC-36** — [`tm manager`: Layer-3 Chat-Based Portfolio Project Manager](./tm-manager-vision.md)
- **DOC-35** — [`tm project`: Deterministic Project/Session Control Plane](./tm-project-control-plane.md)
- **DOC-39** — [trusty-code Harness UI](./trusty-code-harness-ui.md)
- **DOC-40** — [Durable Background Agents](./durable-background-agents.md)
- **DOC-38** — [Spec-Linked Documentation](./spec-linked-documentation.md)
- **Issue #2109** — `tm manager`: Layer-3 Chat-Based Portfolio Project Manager (epic)
- **Issue #2983** — trusty-code REST API Phase 2 (session routes)

---

## 12. Glossary

| Term | Meaning |
|---|---|
| **Lead** | The persistent "engineering lead" or "virtual twin" agent. Holds intent, supervises workstreams, makes confident decisions. |
| **Workstream** | A unit of work (ticket) assigned to a tool and lived out in one or more sessions within that tool. |
| **Liaison** | A per-workstream supervision agent. Watches one workstream, emits summaries, executes lead directives. |
| **Connector** | A stateless tool for controlling sessions (create, status, send, attach, delegate). Implemented per tool (tm, tcode). |
| **Confidence gate** | State machine (Closed/Open/HalfOpen) that decides whether the lead acts autonomously or escalates. |
| **Escalation** | Lead asks user for input (decision is low-confidence). User answer refines the confidence model. |
| **Audit trail** | Durable log of every decision, confidence score, user response, and outcome. |
| **Tool assignment** | Ticket-level choice (tm or tcode). Immutable for a workstream's lifetime. |
