---
spec_refs:
  - id: SPEC-WS-02~draft
    path: docs/specs/DOC-48-tcode-workstreams.md
    anchor: SPEC-WS-02~draft
  - id: SPEC-WS-04~draft
    path: docs/specs/DOC-48-tcode-workstreams.md
    anchor: SPEC-WS-04~draft
  - id: SPEC-TCUI-08~draft
    path: docs/specs/trusty-code-harness-ui.md
    anchor: SPEC-TCUI-08~draft
---

# DOC-52 — Shared Workstream Definition: Cross-Harness Session Binding and Resource Governance

**Status:** Draft (Rev 1)
**Subsystem:** trusty-tools — cross-harness (trusty-mpm, trusty-code, trusty-agents) workstream semantics and lifecycle
**Owner:** Engineering (trusty-mpm, trusty-code, trusty-agents coordination)
**Last-updated:** 2026-07-22 (Rev 1: owner-approved 1:1 session binding, resource caps, scope-overlap rules)
**Spec ID:** `SPEC-SHAREDWS-01~draft` … `SPEC-SHAREDWS-04~draft` (DOC-52)
**Builds on:**
- [`docs/specs/DOC-48-tcode-workstreams.md`](./DOC-48-tcode-workstreams.md) — the daemon-scoped, single-active workstream model for trusty-code (foundation)
- [`docs/specs/trusty-code-harness-ui.md`](./trusty-code-harness-ui.md) (DOC-39) — defines workstream as "the unit of work… an infinite thread with state `active · idle · closed` that you *pick up*, never 'start over'" (product definition)
- [`docs/adr/0019-unified-ipc-messaging-on-event-bus.md`](../adr/0019-unified-ipc-messaging-on-event-bus.md) — workstream-keyed addressing for cross-PM messaging (ADR-0019)
- [`docs/adr/0016-orchestration-hierarchy-lead-pm-assistant.md`](../adr/0016-orchestration-hierarchy-lead-pm-assistant.md) (ADR-0016) — durable orchestration hierarchy using workstream/role identity
- Issue #3649 (session-owned worktrees) and #2919 (auto-reclamation) — resource lifecycle tied to workstream/session ownership

**Supersedes/Clarifies:**
- DOC-48 §2.1 and §4.1 (session-binding model) — **OVERRIDES** DOC-48's append-only `session_ids: Vec<SessionId>` (which permits many sessions per workstream over time, per DOC-48 §2.1 line 81 and Phase C++ code) with a stricter 1:1 invariant per the 2026-07-22 owner decision, effective for new workstreams going forward. Persisted schema compatibility: keep the `session_ids: Vec` field shape; new workstreams enforce `len(session_ids) == 1`; existing arrays with >1 entries are grandfathered as historical lineage (read-only, never migrated destructively).
- DOC-39 §3.1 ("an infinite thread") — clarifies boundary conditions: when a workstream closes, its session is terminal; a closed workstream is NOT reopened (new work creates a new workstream)

---

## 1. Motivation: Owner-Approved Session Binding Model {#SPEC-SHAREDWS-01~draft}

**ID:** SPEC-SHAREDWS-01~draft
**Status:** Draft

### 1.1 The override — DOC-48's many-sessions model vs. 1:1 binding

DOC-48 §1.2 and §2.1 establish that **workstreams are NOT sessions** (DOC-48 §1.2, line 54). DOC-48 §2.1 and §4.1 define the domain model: workstreams hold an append-only `session_ids: Vec<SessionId>` — "Sessions are never removed from the list; only new ones are added" (DOC-48:81). Phase C++ shipped code (`pickActiveSessionInWorkstream`, DOC-48:593) selects among multiple sessions in that vector.

**This spec OVERRIDES that model.** DOC-48's append-only-many semantics were the original design; the 2026-07-22 owner decision mandates a stricter, backward-incompatible change: exactly ONE session per workstream, created and destroyed together, effective for new workstreams going forward.

**Why the override was necessary:**
- **Resource lifecycle clarity:** Who owns the worktree (issue #3649)? With many sessions per workstream, the answer is ambiguous. With 1:1, the session owns the worktree for its lifetime; workstream closure is the reclamation trigger.
- **Auto-reclamation:** Issue #2919 requires a clear close-trigger. With 1:1, closing the workstream closes the session and triggers cleanup. With many sessions, cleanup semantics are underspecified.
- **Cross-PM messaging:** ADR-0019 specifies workstream-keyed addressing. With 1:1 binding, "address this workstream" unambiguously means "address its one session." With many sessions, the route is unclear.

**Schema compatibility consequence:** Persisted workstreams may already have `session_ids` arrays with >1 entries (created under the old model). These are grandfathered as historical artifacts (read-only; never migrated destructively). New workstreams enforce the 1:1 invariant: `len(session_ids) == 1` at all times.

**Owner decision (2026-07-22, stated precisely):**

> **There is exactly ONE session per workstream — the session is the workstream's persistent runtime substrate; they are created and destroyed together.** Access uses tmux semantics: users/clients *connect* to an active workstream; any connection can propel it (send input/drive work), and the resulting state is reflected across ALL active connections simultaneously (multi-client mirror, like several tmux clients attached to one tmux session). "Single-user" = one owning user; that user may hold N concurrent connections (GUI, terminal, channels). Connections never own anything.

This spec is the normative statement of that decision for both trusty-mpm and trusty-code.

### 1.2 Scope — the shared concept

**This spec defines workstream as a shared concept** used identically by trusty-mpm and trusty-code. The invariants and resource rules below apply to both:

- **trusty-mpm PM sessions:** a session lives in a worktree working on one branch; a workstream is the persistent harness construct that owns that worktree and session, and closes when the branch is merged
- **trusty-code daemon sessions:** a session is one turn thread in the daemon's registry; a workstream is the durable named container that holds it; closing the workstream is the trigger for worktree reclamation

Both harnesses may implement these invariants differently (different storage layers, different UI surfaces), but the **workstream semantics and lifecycle are unified.**

---

## 2. Workstream-Session Binding and Invariants {#SPEC-SHAREDWS-02~draft}

**ID:** SPEC-SHAREDWS-02~draft
**Status:** Draft

### 2.1 Binding cardinality (1:1, immutable)

**Invariant:** Exactly ONE session binds to each workstream at any instant. The binding is immutable over the session's lifetime.

- **Creation:** When a workstream is created (explicitly or implicitly per DOC-48 §8 Phase C++), exactly one session is minted and bound to it. The session's lifetime is tied to the workstream's.
- **No rebinding:** A session never changes its workstream binding. If a user closes a workstream and later wants to open new work, they create a NEW workstream (and a new session binds to it).
- **No sharing:** Multiple sessions NEVER bind to one workstream. At any instant, there is one session per workstream, and one workstream per session (with the exception of orphaned closed workstreams, which accept no new sessions by definition).
- **Closure:** When a workstream closes, its session is also terminal. The session may accept trailing turns (to drain a backlog or allow a graceful shutdown message), but it cannot transition to a new workstream.

### 2.2 Access model — multi-client mirror

**Session runtime is single-writer, multi-connection observer.**

- **One owning user:** The session's work is driven by a single user (the workstream's owner).
- **Multiple clients/connections:** That user may hold multiple concurrent connections to the session — a GUI tab, a CLI client, a relay to Slack, etc. Each connection can send input; the resulting state is reflected across ALL active connections simultaneously (tmux-like behavior).
- **No exclusive leases per connection:** Unlike DOC-40's per-agent `AttachmentLease` model (exclusive lease per client), workstream sessions use daemon-enforced singleton activation (DOC-48 §6) or per-harness equivalents. Multiple clients observing the same session do NOT require exclusive leases.
- **Connections never own the session:** Connections are ephemeral; the session owns itself. Closing a connection does not close the session or the workstream.

### 2.3 Lifecycle — created and destroyed together

A workstream and its session share a lifecycle:

| Phase | Action | Meaning |
|-------|--------|---------|
| **Creation** | Workstream is minted (explicitly or implicitly) | One session is bound to it; both enter `active` state if immediately activated, or `idle` if deferred |
| **Active work** | Session accumulates turns; workstream tracks session activity | Session may receive turns; workstream state reflects this |
| **Closure** | Workstream is explicitly closed (via `workstream.close` in tcode, or branch merged in tm) | Session is marked terminal; accepts no new turns (may drain backlog); both are considered closed |
| **Closed state** | No new work accepted | Neither the workstream nor its session accepts new turns. Historical review only. |
| **Reclamation** | Workstream close triggers resource cleanup (issue #2919) | Worktree (tm), build artifacts (trusty-code), and any tied resources are reclaimed per #3649's session-ownership model |

### 2.4 Relationship to sessions (clarification)

**Workstreams and sessions are complementary, not synonymous.**

- **Session** (trusty-memory, trusty-mpm, trusty-code) — a transcript with a `turn_id`, persisted and queryable. Infinite in principle; used for history and memory recall.
- **Workstream** — a named, durable grouping and lifecycle container for the session. Finite; opens, runs, closes. Its closure is the trigger for resource reclamation.

A workstream "owns" a session in the sense that the workstream's lifecycle governs the session's. When the workstream closes, the session is terminal. When the workstream is created, a session is minted.

---

## 3. Resource Rules and Governance {#SPEC-SHAREDWS-03~draft}

**ID:** SPEC-SHAREDWS-03~draft
**Status:** Draft

### 3.1 Concurrency caps on OPEN workstreams

**Default:** 3 open workstreams per project; 8 open workstreams per machine. Configurable per project or globally.

**Rationale:** A worktree with a Rust target/ directory costs ~14 GB on average. An uncontrolled proliferation of open workstreams can exhaust disk space (a 1.1 TB leak occurred 2026-07-21 from orphaned worktrees). Concurrency caps force intentional closure (merging and cleanup) before opening new work.

**Behavior:**
- When a workstream is opened (activated or created if not yet active) and the cap would be exceeded, the operation is REFUSED by default.
- Optional override: user may explicitly pass `--force` to override the refusal and open the workstream anyway. This is a deliberate, visible choice (not silent).
- Per-project disk quota (optional, not in Phase 1): a secondary refusal gate based on projected worktree + target usage (default ~100 GB).

### 3.2 Scope declaration and overlap gate

**Every workstream carries a scope** — a set of path globs (in a monorepo: crate names, package paths, or directory patterns). The scope is inferred from the driving issue or first commits and is refinable by the operator.

**Scope inference:** The workstream's first task or the first commit determines the initial scope. For example, if the first commit touches `crates/trusty-search/src/`, the scope is inferred as `crates/trusty-search/**`. If multiple crates are touched, the scope is the union.

**Overlap gate (monorepo coordination):**
- When a new workstream is opened and its scope OVERLAPS an already-OPEN workstream's scope, the operation is REFUSED by default.
- **Rationale:** Monorepo CI and merge ordering demand clear scope separation. Two workstreams touching the same crate risk merge conflicts, lockfile races, and ordering hazards.
- **Override:** User may explicitly acknowledge the overlap with `--accept-conflict={workstream_id}`, making the intent visible.

**Expected-conflict files (exempt from overlap gate):**
- Root `Cargo.toml`, `Cargo.lock`, lockfiles, `CHANGELOG.md`, generated artifacts (build outputs, vendored deps) are touched by many workstreams by necessity.
- These files are exempt from the overlap gate (never refuse on overlap).
- Accessing them WARNS and auto-widens the workstream's scope (with confirmation).

### 3.3 Merge minimization and staleness nudges

**Staleness nudges:**
- When a workstream's branch falls **>20 commits behind** `main` (configurable), operators receive a nudge to rebase or merge.
- Idle workstreams (no turns received) **>7 days** are flagged as close-or-justify candidates.
- No enforcement; nudges are informational.

**Merge order:**
- Smallest-scope-first: workstreams with narrower scopes (fewer files touched) merge before broader ones, reducing downstream conflict.
- Lockfile/generated-file touchers rebase after each sibling merge to keep their versions current.
- One PR (or one stacked series) per close — no indefinite partially-merged workstreams.

### 3.4 Close triggers reclamation

**Closing a workstream is THE trigger for resource cleanup.**

**Stable workstream-ownership mapping:** Issue #3649 (session-owned worktrees) requires ownership tracking via `owner_session_id` sentinel. Because the 1:1 binding invariant (§2.1) establishes exactly one session per workstream, workstream_id is the stable durable key; `owner_session_id` resolves to it through an explicit 1:1 mapping maintained at close time. This mapping satisfies ADR-0019's "never key on fragile session_id" principle (ADR-0019 forbids `session_id`-keyed addressing to avoid #3396 pane-ID drift): `workstream_id` is the permanent identifier; the session binding it points to may change over the workstream's lifetime (only at creation and closure), but the mapping is always consistent with the workstream's state.

**Reclamation process (issue #2919):**
- Closing a workstream triggers the auto-reclamation of its worktree (trusty-mpm) or build artifacts (trusty-code).
- Worktree deletion is owner-gated via #3649's sentinel (`owner_session_id` → `workstream_id` mapping); no unowned worktrees are deleted.
- Reclamation is non-destructive at the git level — the branch may remain in the remote (for audit/history); the local worktree and its artifacts are cleaned up.

---

## 4. Acceptance Criteria and Conformance {#SPEC-SHAREDWS-04~draft}

**ID:** SPEC-SHAREDWS-04~draft
**Status:** Draft

### AC-1: Shared binding model (1:1 session↔workstream)

**AC-1.1** Exactly ONE session binds to each workstream. The binding is established at workstream creation and is immutable.

**AC-1.2** When a workstream is closed, its session is marked terminal. No new turns are accepted (sessions may drain backlog per harness semantics, but no new work propels the session).

**AC-1.3** A closed workstream is NOT reopened. New work after closure creates a new workstream and a new session.

**AC-1.4** Multiple clients/connections may observe and drive the same session simultaneously (tmux multi-attach behavior). Connections are ephemeral; the session persists independently.

### AC-2: Resource governance

**AC-2.1** Concurrency caps (default 3 per project, 8 per machine) are enforced. Opening a workstream beyond the cap is refused unless `--force` is passed.

**AC-2.2** Scope declaration is inferred at workstream creation and is refinable. Scope overlap between OPEN workstreams is detected and refused by default; override requires explicit `--accept-conflict={id}`.

**AC-2.3** Expected-conflict files (root Cargo.toml, lockfiles, CHANGELOG) are exempt from overlap gate. Touching them widens scope with confirmation.

**AC-2.4** A commit touching files outside the declared scope (not on the expected-conflict list and not overlapping another open workstream's scope) WARNS but is never refused. Scope warnings encourage explicit scope refinement.

**AC-2.5** Staleness nudges (>20 commits behind main, >7 days idle) are informational. No enforcement.

**AC-2.6** Workstream closure triggers auto-reclamation (worktree cleanup for trusty-mpm; artifact cleanup for trusty-code).

### AC-3: Cross-harness semantics

**AC-3.1** trusty-mpm and trusty-code both adhere to the 1:1 binding model defined in §2.

**AC-3.2** Workstream-keyed addressing (ADR-0019) resolves to the single session bound to that workstream. There is no ambiguity about "which session" a message addresses.

**AC-3.3** The #3649 session-ownership sentinel (`owner_session_id`) maps to `workstream_id` via the 1:1 binding invariant. At close time, `owner_session_id` resolves to the workstream that owns it (1:1 cardinality guarantee). Reclamation uses the workstream identity (not the fragile session_id) as the stable ownership key, satisfying ADR-0019's stable-key principle.

---

## 5. Out of Scope

- **Multi-user workstreams:** This spec addresses single-operator workstreams. Team collaboration is future scope.
- **Cross-project workstreams:** Workstreams are project-scoped (one project per workstream in trusty-code; one branch per workstream in trusty-mpm).
- **Enforcement implementation:** Concurrency caps, scope overlap, and staleness nudge logic are specified as behavior contracts; implementation details (where the checks live, how they're surfaced, CLI/API shape) are implementation follow-ups.
- **Migration of existing workstreams:** Existing workstreams (pre-this spec) that violate 1:1 binding (e.g., sessions previously rebound to different workstreams) are not retroactively migrated. The spec applies to new workstreams; existing ones deprecate gracefully.

---

## 6. Relationship to Other Specs

| Spec | Relationship |
|---|---|
| DOC-48 (tcode Workstreams) | **OVERRIDES** DOC-48's append-only `session_ids: Vec` model (§2.1, §4.1) with 1:1 binding, effective for new workstreams. DOC-48's daemon-scoped activation (§6) and SSE fan-out (§5.3) remain unchanged; only the session-binding cardinality is overridden here. Existing persisted arrays with >1 entries are grandfathered (read-only). DOC-48 §11 should add a normative note that its append-only-many semantics are superseded for new workstreams by DOC-52. |
| DOC-39 (trusty-code Harness UI) | Defines workstream as "the unit of work… an infinite thread… you pick up." This spec makes precise what "infinite" means in a 1:1 session-binding context: infinite turns within one session; new work after closure starts a new workstream. |
| ADR-0019 (Unified IPC) | Specifies workstream-keyed addressing for cross-PM messaging. This spec's 1:1 binding eliminates the "which session?" ambiguity in ADR-0019's message routing. |
| #3649 (Session-owned worktrees) | Proposes session-id-keyed ownership via `owner_session_id` sentinel. This spec equates that with workstream_id internally, settling how the two identifiers relate. |
| #2919 (Auto-reclamation) | This spec makes workstream closure the reclamation trigger (§3.4), tying cleanup to workstream lifecycle rather than an arbitrary TTL or timeout. |

---

## 7. Verification and Examples

### Example 1: Single operator, multiple concurrent projects

Alice is working on trusty-mpm (Project A) and trusty-code (Project B) concurrently.

- **Workstream A-1:** "Feature X for trusty-mpm" — opens a session, binds exactly one session to it. Alice opens a GUI tab and a CLI connection; both connect to the same session and see the same state.
- **Workstream B-1:** "Feature Y for trusty-code" — opens a session, binds exactly one session to it. Alice switches tabs; the active workstream changes.
- **Closure:** Alice merges Feature X. Workstream A-1 closes, its session is marked terminal, the worktree is reclaimed. Alice creates **Workstream A-2** (not A-1 reopened) for the next task.
- **Concurrency:** Alice has 2 open workstreams (A-1 is still open until merged; B-1 is open). The cap is 3 per project, so A-2 opens freely. If she opens A-3 while A-1 is still open, A-1 and A-2 are live, the cap would be exceeded: refused unless `--force`.

### Example 2: Scope overlap detection

Alice opens a workstream to fix a bug in `crates/trusty-search/src/indexer.rs`.

- **Workstream: "Fix indexer crash"** — scope inferred as `crates/trusty-search/**`.
- Alice later opens **Workstream: "Refactor embeddings"** — scope inferred as `crates/trusty-search/**` (overlaps).
- **Refusal:** "Cannot open workstream 'Refactor embeddings': scope overlaps with open workstream 'Fix indexer crash' (crates/trusty-search/**). Pass `--accept-conflict=<id>` to override."
- Alice either: (a) closes the first workstream, then opens the second, or (b) acknowledges the conflict and opens both concurrently, accepting the merge/rebase burden.

### Example 3: Staleness and closure

Alice opened **Workstream: "CI optimization"** 10 days ago. It's been idle for 8 days (no turns since day 2).

- **Nudge:** "Workstream 'CI optimization' (2026-07-12) has been idle 8 days. Ready to close? If continuing, update the task or resume the workstream."
- Alice may: (a) close it (merging if needed), triggering reclamation, or (b) reactivate it by revisiting it, adding a new turn, which resets the idle counter.

---

## 8. Non-goals

- **Retroactive migration of many-sessions workstreams:** Existing workstreams that violate 1:1 binding are not forcibly refactored; they deprecate as the system moves to 1:1 semantics.
- **Enforcement mechanism details:** This spec defines *what* to enforce (caps, overlap gate, staleness nudges). *How* (CLI flags, API shape, error messages) is implementation scope.
- **Team collaboration:** This spec is single-operator only.
- **Cross-project sessions:** Workstreams do not span projects.

---
