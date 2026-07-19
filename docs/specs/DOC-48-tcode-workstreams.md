---
spec_refs:
  - id: SPEC-WS-01~draft
    path: docs/specs/DOC-48-tcode-workstreams.md
    anchor: SPEC-WS-01~draft
  - id: SPEC-WS-02~draft
    path: docs/specs/DOC-48-tcode-workstreams.md
    anchor: SPEC-WS-02~draft
  - id: SPEC-WS-03~draft
    path: docs/specs/DOC-48-tcode-workstreams.md
    anchor: SPEC-WS-03~draft
  - id: SPEC-WS-04~draft
    path: docs/specs/DOC-48-tcode-workstreams.md
    anchor: SPEC-WS-04~draft
  - id: SPEC-WS-05~draft
    path: docs/specs/DOC-48-tcode-workstreams.md
    anchor: SPEC-WS-05~draft
  - id: SPEC-WS-06~draft
    path: docs/specs/DOC-48-tcode-workstreams.md
    anchor: SPEC-WS-06~draft
---

# DOC-48 — tcode Workstreams: Durable Named Work Aggregation

**Status:** Draft
**Subsystem:** trusty-code — workstream persistence, session lifecycle, RPC/REST/CLI surfaces
**Owner:** Engineering (trusty-code)
**Last-updated:** 2026-07-19
**Spec ID:** `SPEC-WS-01~draft` … `SPEC-WS-06~draft` (DOC-48)
**Builds on:**
- [`docs/specs/trusty-code-harness-ui.md`](./trusty-code-harness-ui.md) (DOC-39, merged) — §3.1 and §4B define the Workstream domain object as a NEW prerequisite: "an infinite thread with state `active · idle · closed`… resumable across daemon restarts." This spec is the Phase 2+ implementation of that domain object and its persistence layer.
- [`docs/specs/durable-background-agents.md`](./durable-background-agents.md) (DOC-40, merged) — §5 defines the `AttachmentLease` pattern for exclusive attach/detach semantics on background agents. This spec extends that pattern one level up: a per-workstream lease enforces that only one client may be actively connected to a workstream at any instant.
- [`docs/specs/spec-linked-documentation.md`](./spec-linked-documentation.md) (DOC-38, merged) — §4 normalizes the spec-authoring conventions (DOC-N numbering, header block, anchors, catalog) used here.
- [`docs/trusty-code/vision-and-architecture-spec.md`](../trusty-code/vision-and-architecture-spec.md) — Axiom 2: "The workstream is the unit of work" (not a session; many per project; durable across daemon restart).

**Cross-ref (unmerged, cited by name):**
- [`spec-twin-lead-architecture` branch](https://github.com/bobmatnyc/trusty-tools/tree/spec-twin-lead-architecture) — DOC-44 (Engineering Lead Twin Orchestration) defines the cross-tool ledger that will reference tcode workstream IDs. DOC-48 is this spec; a DOC-44 ledger entry for `Harness::Tcode` will hold a `workstream_id` and bind it to tcode's native workstreams (this spec's domain object).

> **Scope note.** This is a **functional spec**, not an implementation plan. It states what the product must do — domain objects, storage layout, state transitions, persistence reconciliation, lease/exclusivity semantics, API surface (RPC/REST/CLI), and acceptance criteria — without prescribing the exact Rust types or storage medium. Per DOC-39 §2.1's binding layer-priority rule (API → CLI → TUI → Web), §5 (the RPC/REST surface) is the normative core; everything else is downstream of it. The PR carrying this doc opens **no** Rust changes.

---

## 1. Motivation and problem statement {#SPEC-WS-01~draft}

**ID:** SPEC-WS-01~draft
**Status:** Draft

### 1.1 The collision — workstreams are infinite, the registry is not

DOC-39 §3.1 introduces **Workstream** as a NEW domain object: "the unit of work… an infinite thread with state `active · idle · closed` that you *pick up*, never 'start over'… MUST survive daemon restart."

Yet DOC-39 §4B acknowledges the collision:

> **The collision.** "The workstream never ends" (principle 1) sits directly on top of a `SessionRegistry` that is a `HashMap` behind a `Mutex`, whose module doc states persistence is **"(Phase 2+, out of scope)"** (`crates/trusty-code/src/session/registry.rs:1-12`). **A daemon restart loses the session list.** trusty-memory retains the *turn history* via the dual-write — so the paradox today is that the **conversation outlives the workstream that contained it**.

A workstream has **no daemon-owned identity** today. The in-memory `SessionRegistry` (`crates/trusty-code/src/session/registry.rs`) is the sole record; on restart it is discarded. A user cannot list prior workstreams, resume one by name, or distinguish "the workstream has not started yet" from "the workstream existed but the daemon restarted."

This is the **single blocking item in DOC-39 Phase 2** (§6.3 flags it as the largest item in the whole spec).

### 1.2 Scope — workstreams are NOT sessions

**Workstreams are NOT sessions.** This is the defining distinction:

- **Session** (DOC-39 §3, DOC-40 context) — a `turn_id`-keyed transcript in trusty-memory's MPT log. Persisted. Infinite. One per code interaction. The PM's running conversation.
- **Workstream** (DOC-39 §3.1, this spec) — a named, durable grouping mechanism for sessions. Many per project. Durability is over a per-project store (`~/.trusty-code/workstreams/`), not over the transcript. Sessions attach to workstreams; workstreams do not replace sessions.

The memory note `"Agents survive pause — check before re-dispatch"` warns that pre-pause agents may still be alive; workstreams are the complementary durable home for that work — a workstream survives pause, can be resumed, and sessions within it can be re-attached.

---

## 2. Domain model {#SPEC-WS-02~draft}

**ID:** SPEC-WS-02~draft
**Status:** Draft

### 2.1 Workstream (the domain object)

A **Workstream** is a durable named grouping of sessions, tied to a project, with mutable state inferred from session activity.

| Field | Type | Mutability | Semantics |
|---|---|---|---|
| **id** | `UUID` | Immutable | Unique per workstream. Generated on creation. Used as the primary key in storage and RPC (`workstream.get`, `workstream.attach`, etc.). |
| **project_id** | `ProjectId` | Immutable (at creation) | Which project (e.g. `acme-api`) this workstream belongs to. Once set, never changes. Workstreams are project-scoped; no cross-project sessions. |
| **name** | `String` | Mutable | Human-readable name, inferred from the first task ("Token rotation hardening") but user-editable (future scope: Phase C, out of scope here). Operator may rename a workstream. |
| **state** | `Enum: active \| idle \| closed` | Computed | **Inferred** from session activity; operator does not set it. See §2.2. |
| **session_ids** | `Vec<SessionId>` | Append-only | Sessions that have been bound to this workstream. A session is created and **optionally binds to** a workstream (§4.1); if it does, its ID is appended to this list. Sessions are never removed from the list; only new ones are added. |
| **created_at** | `Timestamp` | Immutable | When the workstream was created (first `workstream.create` call). |
| **updated_at** | `Timestamp` | Mutable | Last mutation: state inferred, name changed, session attached, workstream closed. Reflects liveness. |
| **metadata** | `Map<String, Value>` | Mutable (optional) | Reserved for future use (tags, description, archived flag, etc.). Out of scope for Phase 1; storage layout reserves it. |

### 2.2 Workstream state — inferred, never set by operator

Workstream state is **computed and immutable from the operator's perspective**. The operator never calls `workstream.set_state(…)`. State is inferred as follows:

- **`idle`** (default initial state) — Created, but no sessions yet attached, or all attached sessions are idle (no recent turns, no running tasks).
- **`active`** — At least one session is active (has a running task or has had a turn in the last N minutes; see §3.2 for TTL).
- **`closed`** — Operator explicitly called `workstream.close` (see §5.2). Once closed, a workstream accepts no new sessions and renders as a historical artifact in the switcher.

State transitions are **monotonic**: `idle → active` (when a session starts work), `active → idle` (all sessions quiet for TTL), `active/idle → closed` (explicit close), `closed → closed` (terminal).

### 2.3 Project binding

DOC-39 §4.2 specifies three project-binding states for a session. A workstream **narrows this to one**:

A workstream **binds to exactly one project at creation** (the `project_id` field). That binding is **immutable**. All sessions attached to a workstream must bind to the same project. If a `task.run` arrives with a different project, it is rejected (see §4.1 and §5 AC-1.3).

---

## 3. Persistence and boot reconciliation {#SPEC-WS-03~draft}

**ID:** SPEC-WS-03~draft
**Status:** Draft

### 3.1 Storage layout

Workstreams are persisted in the user's trusty-code data directory as **JSON records**, one per workstream:

```
~/.trusty-code/workstreams/
├── {project_id}/
│   ├── {workstream_id}.json        # Workstream record
│   ├── {workstream_id}.json
│   └── …
└── …
```

**File naming:** `{project_id}/{workstream_id}.json`, where:
- `project_id` is the project's canonical identifier (a hash, path, or name TBD by the project-binding semantics; must be filesystem-safe).
- `workstream_id` is the workstream's UUID, stringified and lowercased (e.g. `a1b2c3d4-e5f6-4748-9a0b-1c2d3e4f5a6b.json`).

**Record schema (JSON, minimal example):**

```json
{
  "id": "a1b2c3d4-e5f6-4748-9a0b-1c2d3e4f5a6b",
  "project_id": "acme-api",
  "name": "Token rotation hardening",
  "state": "idle",
  "session_ids": ["sess-001", "sess-042"],
  "created_at": "2026-07-19T14:32:00Z",
  "updated_at": "2026-07-19T15:45:30Z",
  "lease": {
    "holder_id": "uuid-of-attached-client",
    "holder_kind": "Operator",
    "acquired_at": "2026-07-19T15:40:00Z",
    "lease_expires_at": "2026-07-19T15:41:00Z",
    "connection_token": "opaque-string"
  },
  "metadata": {}
}
```

The `lease` field is optional; see §3.3.

### 3.2 Idle-state TTL

**Active-to-idle transition TTL:** A workstream in `active` state transitions to `idle` if **all** its sessions have not had a new turn for **5 minutes** (configurable). The daemon computes this at each turn boundary (when inferring state for the switcher list, or via explicit `workstream.list` call).

This is a **read-side computation**, not persisted state. It is never stored in the JSON record — only `updated_at` is recorded, and the TTL is applied when reading.

### 3.3 Boot reconciliation

When `tcode serve` starts, it **reconciles the stored workstream records against live daemon state**:

1. **Load all workstream records** from `~/.trusty-code/workstreams/{project_id}/*.json`.
2. **Load all live session IDs** from the `SessionRegistry` (or the session's own durable store if it exists; see DOC-39 §4B).
3. **For each workstream record:**
   - **If `session_ids` is empty:** mark as `idle`, clear any stale `lease` (see §3.4).
   - **If `session_ids` is non-empty:** check if any session is live (in the registry). If yes, infer state; if no, mark as `idle` and clear stale `lease`.
   - **Clear stale leases:** if `lease_expires_at` has passed, set `lease` to `null` (see §3.4). A client may later `acquire` a workstream with a stale lease by calling `acquire{force: false}` (the default), which succeeds because there is no live holder.

4. **Persist the reconciled state** (only if changes were made, to avoid unnecessary writes).

This reconciliation is **non-destructive**: no workstream record is deleted, only re-initialized if it lost all sessions. The user always sees their prior workstreams.

### 3.4 Lease staleness

Leases are **in-memory during the daemon's lifetime** and do **not survive** a daemon restart (leases are a live-connection concept, not durable state). On restart:

- All `lease` fields in stored records are **cleared to `null`** (or cleared during reconciliation).
- A client's first `workstream.attach` after restart treats the workstream as unattached and succeeds (via `acquire{force: false}`), as if the prior lease had expired.

This design mirrors DOC-40 §5.4: "Lease is not durable across restart… because there is no record left for `acquire` to conflict with."

**Per-TTL staleness (within a daemon lifetime):** If a holder (a client) fails to call `heartbeat` before `lease_expires_at`, the lease record is retained but marked as stale. A new client may then:
- Call `acquire{force: false}` (default) → succeeds, because the lease is stale (no live holder).
- Call `acquire{force: true}` (take-over) → also succeeds; this is for explicit eviction (see §5.2).

---

## 4. Workstream lifecycle {#SPEC-WS-04~draft}

**ID:** SPEC-WS-04~draft
**Status:** Draft

### 4.1 Session binding

A session **optionally binds to a workstream** at creation (via `session.create` or `task.run`). The binding is **immutable over the session's lifetime**:

```rust
// Pseudocode — illustrative only
session.create(
  project_id: Option<ProjectId>,
  workstream_id: Option<WorkstreamId>,  // NEW binding
  …
)
```

**Rules:**
- **If no `workstream_id` is passed**, the session is **projectless** (valid; see DOC-39 §4.2).
- **If `workstream_id` is passed**, the session binds to that workstream and inherits its `project_id`. The inherited project overrides any per-call `project` argument (see DOC-39 AC-21.7).
- **If a session already exists with a binding**, and a new turn arrives with a different workstream, the turn is **rejected** (workstreams are immutable per session).
- **If a workstream is closed**, new sessions may **not** bind to it. Existing sessions in that workstream remain valid and may accumulate turns (though the closed workstream will not appear in the switcher).

### 4.2 Ambient default target

When a client is **attached to a workstream** (via `workstream.attach`), new work dispatched from that client **implicitly targets that workstream** unless overridden per-call.

This is a **future scope** (Phase B, out of scope for Phase 1). The infrastructure is built in Phase 1A, but the UI wiring is deferred.

### 4.3 Workstream closure

A workstream can be explicitly closed via `workstream.close{id}` (see §5.2). Closure is **irreversible**:

- The closed workstream **stops accepting new sessions** (turns attempting to bind are rejected).
- Existing sessions in the workstream **remain valid** (they may accumulate turns).
- The closed workstream **does not appear** in the switcher (7a) or in the default list from `workstream.list`.
- Closed workstreams are **optionally listable** via a `include_closed: true` flag on `workstream.list` (for audit/historical query; default is false).

---

## 5. RPC/REST/CLI surface {#SPEC-WS-05~draft}

**ID:** SPEC-WS-05~draft
**Status:** Draft

### 5.1 JSON-RPC verbs (via `POST /rpc`)

The daemon adds the following JSON-RPC methods (to `crates/trusty-code/src/serve/http.rs` route table):

| Method | Signature (pseudocode) | Purpose |
|---|---|---|
| `workstream.create` | `{project_id: ProjectId} → {id: UUID}` | Create a new workstream. Name is initially empty (or inferred from first task). Returns the workstream ID. |
| `workstream.get` | `{id: UUID} → Workstream` | Fetch a single workstream record (full fields). |
| `workstream.list` | `{project_id?: ProjectId, include_closed?: bool} → Vec<Workstream>` | List all workstreams for a project (or all projects if `project_id` is omitted). Default `include_closed = false`. Returns workstreams with inferred state. |
| `workstream.attach` | `{id: UUID, holder_id?: UUID, force?: bool} → {lease: AttachmentLease, connection_token: String}` | Attach the calling client to a workstream, acquiring an exclusive lease. Default `force = false`. Returns the lease record and a token for use in SSE subscription (§5.3). See §4.2 of DOC-40 for lease semantics. |
| `workstream.detach` | `{id: UUID} → {}` | Release the workstream lease. Client must be the current holder (validated via token). Idempotent — detaching when not attached succeeds. |
| `workstream.heartbeat` | `{id: UUID} → {lease_expires_at: Timestamp}` | Renew the lease TTL. Client must be the current holder. Returns updated expiration. Called by attached client at least once per `lease_ttl / 2` (default: every 30 seconds for a 60-second TTL). |
| `workstream.close` | `{id: UUID} → {}` | Irreversibly close a workstream. Only the attached client may close it. Detaches the caller. |
| `workstream.rename` | `{id: UUID, name: String} → Workstream` | Rename a workstream (future, Phase C). Out of scope for Phase 1A. |

### 5.2 REST endpoints (wrappers around JSON-RPC)

Per #2983 Slice 4, REST alternatives are provided alongside JSON-RPC for one-shot operations:

| Method | Path | Purpose |
|---|---|---|
| `POST` | `/api/v1/workstreams` | → `workstream.create` |
| `GET` | `/api/v1/workstreams/{id}` | → `workstream.get{id}` |
| `GET` | `/api/v1/workstreams?project_id=…&include_closed=…` | → `workstream.list{…}` |
| `POST` | `/api/v1/workstreams/{id}/attach` | → `workstream.attach{id, force?}` |
| `POST` | `/api/v1/workstreams/{id}/detach` | → `workstream.detach{id}` |
| `POST` | `/api/v1/workstreams/{id}/heartbeat` | → `workstream.heartbeat{id}` |
| `POST` | `/api/v1/workstreams/{id}/close` | → `workstream.close{id}` |

All endpoints return JSON. Errors are HTTP 4xx/5xx with a JSON error body (matching existing tcode error conventions).

### 5.3 Server-Sent Events (SSE) for workstream events

When a client is attached to a workstream (via `workstream.attach`), it receives an SSE stream of workstream-scoped events:

```
GET /api/v1/workstreams/{id}/events?token={connection_token}
```

The `connection_token` is returned from `workstream.attach` and is required to subscribe (to prevent unauthorized event leaks).

**Event types (extensible; Phase 1A minimal set):**

| Event | Payload | Meaning |
|---|---|---|
| `SessionAdded` | `{session_id, binding_time}` | A session was bound to this workstream. |
| `SessionActivityUpdate` | `{session_id, last_turn_at, has_running_task}` | Activity status of a bound session (for state inference). |
| `StateInferred` | `{state: active \| idle \| closed, reason: String}` | Workstream state changed (computed). |
| `LeaseRevoked` | `{new_holder_kind: Operator \| Agent}` | Current holder's lease was revoked (another client took over via `attach{force: true}`). Client should disconnect. |

### 5.4 CLI surface — `tcode workstream` family

New CLI commands (in `crates/trusty-code/src/cli/`):

```bash
# List workstreams for current/specified project
tcode workstream list [--project=<id>] [--include-closed]
tcode ws list [--project=<id>] [--include-closed]    # short alias

# Show a single workstream
tcode workstream get <id>
tcode ws get <id>

# Create a new workstream
tcode workstream create --project=<id> [--name=<name>]
tcode ws create --project=<id> [--name=<name>]

# Attach to a workstream
tcode workstream attach <id>
tcode ws attach <id>

# Close a workstream
tcode workstream close <id>
tcode ws close <id>
```

Output format: human-readable table (for list) or JSON (for get). The `tcode ws` short form rhymes with `tm ls` (tcode uses `workstream` as the full term; `ws` is the short alias).

---

## 6. Exclusivity and lease semantics {#SPEC-WS-06~draft}

**ID:** SPEC-WS-06~draft
**Status:** Draft

### 6.1 One client per workstream

**At most one client (UI, CLI, or agent) may be actively attached to a workstream at any instant.** This is enforced by the `AttachmentLease` pattern (DOC-40 §5):

- **`acquire{force: false}` (default):** If the workstream is unattached or has a stale lease, acquire it. If it is attached and the lease is live, return `LeaseConflict` error (include the current holder's kind in the error).
- **`acquire{force: true}` (take-over):** Evict the current holder (if any, and if the lease is live) and install the caller as the new holder. Send the evicted holder a `LeaseRevoked` event.
- **`heartbeat`:** Renew the lease TTL. Caller must be the current holder (validated via token). Default TTL: 60 seconds.
- **`detach` (release):** Voluntarily relinquish the lease. The workstream becomes unattached.

### 6.2 Per-client invariant

A single client (identified by `holder_id`, which may be a user UUID or operator session ID) may hold **at most one workstream lease at a time**.

This is a **soft constraint enforced by policy**, not by the daemon itself. The policy is documented (§7.1) and may be enforced by the UI/CLI layer if desired, but is not a hard technical gate in the RPC layer. (The daemon is the **source of truth** for which client holds a lease, but the client is responsible for not requesting multiple leases simultaneously.)

### 6.3 Conflict resolution for take-over

When a client calls `workstream.attach{id, force: true}` and a live holder exists:

1. The daemon sends the evicted holder a `LeaseRevoked` event over their SSE stream (if connected).
2. The daemon closes their event stream (SSE connection terminates).
3. The new holder's `attach` call succeeds and returns the new lease.

If the evicted holder was not connected (no live SSE stream), the `LeaseRevoked` event is not sent (it is only meaningful for connected clients). The take-over still succeeds silently.

---

## 7. Acceptance Criteria {#SPEC-WS-07~draft}

**ID:** `SPEC-WS-07~draft` (notional; not anchored in text but used for reference)
**Status:** Draft

### AC-1: Core domain and persistence

**AC-1.1** A workstream record has the fields in §2.1 (id, project_id, name, state, session_ids, created_at, updated_at, metadata, lease).

**AC-1.2** Workstreams are persisted to `~/.trusty-code/workstreams/{project_id}/{workstream_id}.json` as JSON.

**AC-1.3** A session may bind to a workstream at creation via `session.create(workstream_id)`. If a session with an existing workstream binding receives a task with a different workstream, the task is rejected.

**AC-1.4** On daemon restart, all workstream records are loaded and leases are cleared (staleness reconciliation, §3.3).

### AC-2: RPC surface

**AC-2.1** All methods in §5.1 exist and are callable via `POST /rpc` (JSON-RPC).

**AC-2.2** All methods in §5.2 exist and are callable via REST (HTTP GET/POST).

**AC-2.3** `workstream.list` returns an array of workstreams with inferred state and grouped by project (when `project_id` is omitted).

**AC-2.4** `workstream.attach` returns a lease record with `holder_id`, `holder_kind`, `acquired_at`, `lease_expires_at`, and `connection_token`.

### AC-3: Exclusivity and leases

**AC-3.1** At most one live lease exists per workstream. Attempts to `attach{force: false}` when a live lease is held return `LeaseConflict`.

**AC-3.2** `workstream.attach{force: true}` evicts the current holder and sends them a `LeaseRevoked` event.

**AC-3.3** `workstream.heartbeat` renews the lease TTL. Calls outside the TTL window are rejected.

**AC-3.4** `workstream.detach` releases the lease. Detaching when not attached succeeds (idempotent).

### AC-4: State inference

**AC-4.1** Workstream state is inferred based on session activity (§2.2) and is never settable by the operator.

**AC-4.2** A workstream transitions `active → idle` if all sessions are quiet for 5 minutes. This is checked at each `workstream.list` call.

### AC-5: CLI

**AC-5.1** `tcode workstream list` works and returns human-readable output.

**AC-5.2** `tcode workstream attach <id>` attaches the CLI client and streams events.

**AC-5.3** `tcode ws` short alias works for all `workstream` commands.

### AC-6: Non-destructive boot reconciliation

**AC-6.1** No workstream records are deleted during boot reconciliation (§3.3).

**AC-6.2** Stale leases are cleared; fresh leases are retained only if the holder is live (this is a read-side check, not persisted).

---

## 8. Phasing {#SPEC-WS-08~draft}

**ID:** `SPEC-WS-08~draft` (notional, not anchored)
**Status:** Draft

### Phase 1A — Domain + persistence + RPC + CLI

- [x] Workstream domain model (§2)
- [x] Storage layout and reconciliation (§3)
- [x] RPC verbs and REST endpoints (§5)
- [x] CLI commands (§5.4)
- [x] Lease model and exclusivity (§6)
- [ ] GUI workstream switcher (deferred to Phase C, blocked on issue #3153 shell rebuild)

### Phase 1B (follow-up, not in this spec) — Session binding UI

- Session-creation UI wiring to bind to workstream (§4.1)
- Ambient default-target logic (§4.2)

### Phase C (future, blocked on #3153) — GUI workstream switcher

- Visual workstream switcher in the header (DOC-39 §2, principle 3)
- State display, rename, close actions
- Project-scoped grouping

---

## 9. Open questions for Bob {#SPEC-WS-09~draft}

**ID:** `SPEC-WS-09~draft` (notional, not anchored)

1. **Q1 — Workstream name inference:** When a session binds to a workstream, should we infer the workstream name from the first turn's prompt text (like DOC-39 suggests), or require the operator to name it at creation? Phase 1A can defer this to Phase 1B (name remains empty or "Untitled" until first session).

2. **Q2 — Project-ID semantics:** How is `project_id` computed? Is it a path hash, a user-friendly name, or an external ID from trusty-mpm's project registry? This affects the storage directory structure and project binding validation (§4.1). Current spec assumes it is filesystem-safe and durable per project.

3. **Q3 — Lease TTL default:** DOC-40 §5.4 proposes 60 seconds default TTL. Is this suitable for tcode, or should it be longer (e.g., 5 minutes) to tolerate sporadic CLI heartbeats? Current spec defaults to 60 seconds; CLI implementation may need adjustment.

4. **Q4 — `update_at` vs. activity-based state:** Should `updated_at` reflect **any** mutation (rename, close) or **only** activity mutations (sessions added, state inferred)? Current spec uses the broader definition (any mutation).

---

## 10. Non-goals and dependencies

- **Not a GUI switchboad (yet).** The workstream switcher visual design (DOC-39 §2) is out of scope. Phase C depends on issue #3153 (shell rebuild).
- **Not cross-machine discovery.** Workstreams are local to one user's `~/.trusty-code/` directory. No multi-machine or team collaboration.
- **Not a DOC-44 ledger owner.** DOC-44 (unmerged) will define the cross-tool ledger; this spec provides the tcode-native workstream domain. The ledger will **reference** tcode workstream IDs, not replace them.
- **Not multi-project sessions.** A workstream binds to one project; a session binds to one workstream. No session may span projects.

---

## 11. Relationship to other specs

| Spec | Relationship |
|---|---|
| DOC-39 (trusty-code Harness UI) | This spec implements §3.1 (Workstream domain) and §4B (durability + persistence). DOC-39 is the consumer; this spec is the provider. |
| DOC-40 (Durable Background Agents) | This spec extends DOC-40's `AttachmentLease` pattern to the workstream level (one lease per workstream, plus a soft per-client invariant). |
| DOC-44 (Engineering Lead Twin Orchestration, unmerged) | DOC-44 will define a cross-tool ledger. This spec defines tcode's native workstream domain; DOC-44 will reference tcode workstream IDs. |
| DOC-38 (Spec-Linked Documentation) | This spec follows DOC-38's header block, anchoring, and catalog conventions. |
| DOC-39's Project binding (§4.2) | Workstreams narrow project binding to one project per workstream (immutable). |

---

## 12. Verification and curl examples

### Setup

Assume a tcode daemon is running on `localhost:7881` (default).

### Create a workstream

```bash
curl -X POST http://localhost:7881/rpc \
  -H "Content-Type: application/json" \
  -d '{
    "jsonrpc": "2.0",
    "id": 1,
    "method": "workstream.create",
    "params": {"project_id": "acme-api"}
  }'

# Response:
# {
#   "jsonrpc": "2.0",
#   "id": 1,
#   "result": {
#     "id": "a1b2c3d4-e5f6-4748-9a0b-1c2d3e4f5a6b"
#   }
# }
```

### List workstreams

```bash
curl -X POST http://localhost:7881/rpc \
  -H "Content-Type: application/json" \
  -d '{
    "jsonrpc": "2.0",
    "id": 2,
    "method": "workstream.list",
    "params": {"project_id": "acme-api"}
  }'

# Response:
# {
#   "jsonrpc": "2.0",
#   "id": 2,
#   "result": [
#     {
#       "id": "a1b2c3d4-...",
#       "project_id": "acme-api",
#       "name": "Token rotation hardening",
#       "state": "idle",
#       "session_ids": [],
#       "created_at": "2026-07-19T14:32:00Z",
#       "updated_at": "2026-07-19T14:32:00Z"
#     }
#   ]
# }
```

### Attach to a workstream

```bash
curl -X POST http://localhost:7881/rpc \
  -H "Content-Type: application/json" \
  -d '{
    "jsonrpc": "2.0",
    "id": 3,
    "method": "workstream.attach",
    "params": {
      "id": "a1b2c3d4-e5f6-4748-9a0b-1c2d3e4f5a6b",
      "force": false
    }
  }'

# Response:
# {
#   "jsonrpc": "2.0",
#   "id": 3,
#   "result": {
#     "lease": {
#       "holder_id": "cli-user-123",
#       "holder_kind": "Operator",
#       "acquired_at": "2026-07-19T15:40:00Z",
#       "lease_expires_at": "2026-07-19T15:41:00Z",
#       "connection_token": "tok_abc123xyz"
#     }
#   }
# }
```

### Heartbeat (renew lease)

```bash
curl -X POST http://localhost:7881/rpc \
  -H "Content-Type: application/json" \
  -d '{
    "jsonrpc": "2.0",
    "id": 4,
    "method": "workstream.heartbeat",
    "params": {
      "id": "a1b2c3d4-e5f6-4748-9a0b-1c2d3e4f5a6b"
    }
  }'
```

---

## 13. Implementation checklist (for code reviewer)

- [ ] Workstream storage directory and JSON schema implemented.
- [ ] Boot reconciliation clears stale leases, loads all records, doesn't delete any.
- [ ] `workstream.create`, `workstream.get`, `workstream.list`, `workstream.attach`, `workstream.detach`, `workstream.heartbeat`, `workstream.close` exist in the RPC router.
- [ ] REST endpoints wrap the RPC methods.
- [ ] CLI commands exist and work with the RPC surface.
- [ ] State inference (idle/active/closed) works based on session activity and TTL.
- [ ] Lease exclusivity is enforced: only one live lease per workstream, `attach{force: true}` evicts.
- [ ] SSE events stream properly (`SessionAdded`, `StateInferred`, `LeaseRevoked`).
- [ ] Session-to-workstream binding is optional at `session.create` and immutable thereafter.
- [ ] Project binding is enforced: sessions in a workstream all bind to the same project.
- [ ] All ACs in §7 pass.
- [ ] All curl examples in §12 succeed.

