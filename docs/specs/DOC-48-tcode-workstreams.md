---
spec_refs:
  - id: SPEC-TCUI-08~draft
    path: docs/specs/trusty-code-harness-ui.md
    anchor: SPEC-TCUI-08~draft
  - id: SPEC-BGATTACH-04~draft
    path: docs/specs/durable-background-agents.md
    anchor: SPEC-BGATTACH-04~draft
  - id: SPEC-BGATTACH-05~draft
    path: docs/specs/durable-background-agents.md
    anchor: SPEC-BGATTACH-05~draft
  - id: SPEC-SLD-02~draft
    path: docs/specs/spec-linked-documentation.md
    anchor: SPEC-SLD-02~draft
---

# DOC-48 — tcode Workstreams: Durable Named Work Aggregation

**Status:** Draft (Rev 2)
**Subsystem:** trusty-code — workstream persistence, session lifecycle, RPC/REST/CLI surfaces
**Owner:** Engineering (trusty-code)
**Last-updated:** 2026-07-19 (Rev 2: activation-lock exclusivity model, single-project binding; Rev 5: unprefixed REST paths)
**Spec ID:** `SPEC-WS-01~draft` … `SPEC-WS-09~draft` (DOC-48)
**Builds on:**
- [`docs/specs/trusty-code-harness-ui.md`](./trusty-code-harness-ui.md) (DOC-39, merged) — [`SPEC-TCUI-08~draft`](./trusty-code-harness-ui.md#SPEC-TCUI-08~draft) §4B defines the Workstream domain object as a NEW prerequisite: "an infinite thread with state `active · idle · closed`… resumable across daemon restarts." This spec is the Phase 2+ implementation of that domain object and its persistence layer.
- [`docs/specs/durable-background-agents.md`](./durable-background-agents.md) (DOC-40, merged) — [`SPEC-BGATTACH-04~draft` and `-05~draft`](./durable-background-agents.md#SPEC-BGATTACH-04~draft) define "never silently multiplex" exclusivity principle at the per-client level. This spec adopts that principle at the daemon level: one workstream is active at a time, and a client attempting to activate a different workstream must explicitly request the switch (never silent).
- [`docs/specs/spec-linked-documentation.md`](./spec-linked-documentation.md) (DOC-38, merged) — [`SPEC-SLD-02~draft`](./spec-linked-documentation.md#SPEC-SLD-02~draft) §2 and §3 normalize the spec reference grammar and SLD conventions used here.
- [`docs/trusty-code/vision-and-architecture-spec.md`](../trusty-code/vision-and-architecture-spec.md) — Axiom 2: "The workstream is the unit of work" (not a session; one active per project; durable across daemon restart). Axiom 4 (daemon owns sessions).

**Cross-ref (merged code):**
- [`crates/trusty-agents-common/src/workstreams/`](../../../crates/trusty-agents-common/src/workstreams/) (trusty-agents PR #3260, merged) — Defines `trusty_agents_common::workstreams::Workstream`, a cross-tool ledger entry type that references tcode workstream IDs and assigned harness. **DISAMBIGUATION:** This spec defines tcode's native workstream domain object (a different type, recommended name: `trusty_code::workstreams::Workstream`). Imports must be qualified in code that touches both: `trusty_agents_common::workstreams::Workstream` for the ledger, `trusty_code::workstreams::Workstream` for tcode-native state.

> **Scope note.** This is a **functional spec**, not an implementation plan. It states what the product must do — domain objects, storage layout, state transitions, persistence reconciliation, activation-lock exclusivity semantics, API surface (RPC/REST/CLI), and acceptance criteria — without prescribing the exact Rust types. Per DOC-39 §2.1's binding layer-priority rule (API → CLI → TUI → Web), §5 (the RPC/REST surface) is the normative core; everything else is downstream of it. The PR carrying this doc opens **no** Rust changes.

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

### 1.2 Scope — workstreams are NOT sessions; one active workstream per daemon

**Workstreams are NOT sessions.** This is the defining distinction:

- **Session** (DOC-39 §3, DOC-40 context) — a `turn_id`-keyed transcript in trusty-memory's MPT log. Persisted. Infinite. One per code interaction. The PM's running conversation.
- **Workstream** (DOC-39 §3.1, this spec) — a named, durable grouping mechanism for sessions. Many per project (per daemon instance). Durability is over a daemon-local store, not over the transcript. Sessions bind to workstreams; workstreams do not replace sessions.

**Exclusivity model:** At any instant, **at most one workstream is active per daemon**. Because `crates/trusty-code/src/serve/mod.rs::build_router(binding: ProjectBinding)` binds ONE project per daemon process, this means one active workstream per project. A client attempting to activate a different workstream fails with `ActiveConflict{active_id}` unless `force: true` (explicit switch, never silent — consistent with DOC-40's "never silently multiplex" principle).

**Multi-client observation:** Unlike DOC-40's per-agent attach/detach leases, workstreams allow **multiple clients to observe and drive the active workstream simultaneously** (like `tmux attach-session` — any number of terminal clients can attach to one session). Clients connect via the daemon's multi-subscriber SSE endpoint; there is no per-client exclusivity gate.

---

## 2. Domain model {#SPEC-WS-02~draft}

**ID:** SPEC-WS-02~draft
**Status:** Draft

### 2.1 Workstream (the domain object)

A **Workstream** is a durable named grouping of sessions, scoped to a single daemon instance, with mutable state inferred from session activity.

| Field | Type | Mutability | Semantics |
|---|---|---|---|
| **id** | `UUID` | Immutable | Unique per workstream. Generated on creation. Used as the primary key in storage and RPC (`workstream.get`, `workstream.activate`, etc.). |
| **name** | `String` | Mutable | Human-readable name, inferred from the first task ("Token rotation hardening") but user-editable (future scope: Phase C, out of scope here). Operator may rename a workstream. |
| **state** | `Enum: active \| idle \| closed` | Computed | **Inferred** from session activity; operator does not set it. See §2.2. One workstream is `active` at a time; others are `idle` or `closed`. |
| **session_ids** | `Vec<SessionId>` | Append-only | Sessions that have been bound to this workstream. A session is created and **optionally binds to** a workstream (§4.1); if it does, its ID is appended to this list. Sessions are never removed from the list; only new ones are added. |
| **created_at** | `Timestamp` | Immutable | When the workstream was created (first `workstream.create` call). |
| **updated_at** | `Timestamp` | Mutable | Last activity: state inferred, name changed, session attached, workstream activated/deactivated, workstream closed. Reflects liveness. |
| **metadata** | `Map<String, Value>` | Mutable (optional) | Reserved for future use (tags, description, archived flag, etc.). Out of scope for Phase 1; storage layout reserves it. |

**Scope note:** Workstreams are **daemon-scoped**, not project-scoped. Because the daemon binds to one project (via `ProjectBinding` in `crates/trusty-code/src/serve/mod.rs`), workstreams are implicitly project-local. The `project_id` field is **not** part of the workstream domain object; it is derived from the daemon's own binding at startup.

### 2.2 Workstream state — inferred, never set by operator; one active per daemon

Workstream state is **computed and immutable from the operator's perspective**. The operator never calls `workstream.set_state(…)`. State is inferred as follows:

- **`idle`** (default initial state) — Created, but no sessions yet attached, or all attached sessions are idle (no recent turns, no running tasks). No other workstream is active.
- **`active`** — This workstream is the **active workstream** (see §4.2); at most one workstream per daemon has this state at any instant. A workstream becomes active when the operator calls `workstream.activate{id}` (see §5.1).
- **`closed`** — Operator explicitly called `workstream.close` (see §5.2). Once closed, a workstream accepts no new sessions and renders as a historical artifact.

**Global invariant (daemon-enforced):** At any instant, **exactly zero or one workstream is `active`** per daemon. A workstream cannot be activated while another is active unless `force: true` is passed (see §5.1, AC-2.2).

### 2.3 Project scoping

All workstreams in a daemon are scoped to the **same project** — the project the daemon was launched for (via `ProjectBinding` in `crates/trusty-code/src/serve/mod.rs::build_router`). This means:

- Workstreams are **per-project** implicitly (one daemon per project).
- All sessions bound to any workstream in a daemon must bind to the daemon's own project.
- There is no per-workstream `project_id` field in the domain object; the project is the daemon's own binding (a runtime constant).

---

## 3. Persistence and boot reconciliation {#SPEC-WS-03~draft}

**ID:** SPEC-WS-03~draft
**Status:** Draft

### 3.1 Storage layout

Workstreams are persisted in the user's trusty-code data directory in a **single flat JSON file per daemon/project**, matched to the daemon's `ProjectBinding`. The storage location is derived from the daemon's project identifier (a filesystem-safe slug + hash from the project path or URL; see below).

```
~/.trusty-code/
└── workstreams-{project_slug}-{hash}.json   # Atomic JSON record of all workstreams + active pointer
```

**Filename:** `workstreams-{project_slug}-{hash}.json`, where:
- `project_slug` is the filesystem-safe short identifier from the project's `ProjectBinding` (e.g., `acme-api` from a project path like `/path/to/acme-api`). A hash suffix disambiguates multiple projects with the same slug.
- The `hash` is a deterministic fingerprint of the project's root path or URL (e.g., MD5 or SHA256, truncated to 8 chars) to handle multiple checkouts of the same project.

This naming scheme **mirrors precedent** in trusty-mpm (sessions.json path derivation) and trusty-agents-common (workstreams.json ledger, PR #3260).

**Record schema (JSON):**

```json
{
  "version": "1.0",
  "active_workstream_id": "a1b2c3d4-e5f6-4748-9a0b-1c2d3e4f5a6b",
  "workstreams": [
    {
      "id": "a1b2c3d4-e5f6-4748-9a0b-1c2d3e4f5a6b",
      "name": "Token rotation hardening",
      "session_ids": ["sess-001", "sess-042"],
      "created_at": "2026-07-19T14:32:00Z",
      "updated_at": "2026-07-19T15:45:30Z",
      "metadata": {}
    },
    {
      "id": "b2c3d4e5-f6a1-4748-9a0b-1c2d3e4f5a6b",
      "name": "Auth refactoring",
      "session_ids": [],
      "created_at": "2026-07-19T13:00:00Z",
      "updated_at": "2026-07-19T13:00:00Z",
      "metadata": {}
    }
  ]
}
```

**Storage semantics:**
- **Atomic writes:** All workstream updates are written to the file via atomic temp-then-rename (write to `.tmp`, then `mv -f .tmp workstreams-….json`). This ensures no partial writes even if the daemon crashes mid-write.
- **Single `active_workstream_id` pointer:** Only one ID here, or `null` if no workstream is active. Persisted and restored on boot.
- **State is not stored:** Workstream `state` (active/idle/closed) is inferred on load, not persisted. See §3.2.

This matches the **dual precedent** of:
1. trusty-mpm's `session_manager/store.rs::SessionStore::save(sessions.json)` — atomic temp+rename, single flat map.
2. trusty-agents-common's `workstreams/ledger.rs::WorkstreamLedger::save(workstreams.json)` — same pattern.

### 3.2 State inference and active workstream restoration

**State computation (read-side, not persisted):**

When the daemon boots or when `workstream.list` is called:
1. Load the persisted `active_workstream_id` from storage.
2. For each workstream:
   - If its ID equals `active_workstream_id`, state is `active` (regardless of session liveness; the activation pointer determines this).
   - Otherwise, state is `idle` (unless marked closed).
   - If the workstream is marked as closed (a metadata flag; see future Phase C), state is `closed`.

**Active workstream restoration on boot:**

When `tcode serve` starts:
1. Load the stored `active_workstream_id` from `workstreams-….json`.
2. Restore that workstream as the active workstream (if it exists).
3. The restored workstream is `active` per the persisted pointer, independent of whether sessions are live.

**This ensures:** A daemon restart **never silently changes which workstream is active**. If the daemon was running workstream A, it resumes with workstream A active (assuming it still exists). If the workstream was deleted, or the user wants to switch, they must explicitly call `workstream.activate{id, force: true}`.

### 3.3 Boot reconciliation

When `tcode serve` starts, it **reconciles the stored workstream records against live daemon state**:

1. **Load workstreams.json** from the file path (§3.1).
2. **Load all live session IDs** from the `SessionRegistry`.
3. **For each workstream record:**
   - Determine state: if its id == `active_workstream_id` pointer, state is `active`; else `idle` (or `closed` if marked closed).
   - Session activity feeds only `SessionActivityUpdate` and `WorkstreamStateInferred` telemetry events; it does NOT determine active/idle state.
   - Do NOT delete any workstream record (non-destructive).

4. **Restore `active_workstream_id`:** Load the stored pointer; if the referenced workstream still exists, restore it as active (see §3.2). If the workstream was deleted, set `active_workstream_id` to `null`.

5. **Persist reconciled state** (only if changes were made, to avoid unnecessary writes).

This reconciliation is **non-destructive**: no workstream record is deleted, only marked as `idle` if it lost all sessions. The user always sees their prior workstreams.

---

## 4. Workstream lifecycle {#SPEC-WS-04~draft}

**ID:** SPEC-WS-04~draft
**Status:** Draft

### 4.1 Session binding

A session **optionally binds to a workstream** at creation (via `session.create` or `task.run`). The binding is **immutable over the session's lifetime**:

```rust
// Pseudocode — illustrative only
session.create(
  workstream_id: Option<WorkstreamId>,  // Optional binding
  …
)
```

**Rules:**
- **If no `workstream_id` is passed**, the session is **projectless/unbound** (valid; see DOC-39 §4.2). It can still run tasks but is not grouped in any workstream.
- **If `workstream_id` is passed**, the session binds to that workstream. The workstream is in the daemon's own project (no cross-project sessions).
- **If a session already exists with a binding**, and a new turn arrives with a different workstream, the turn is **rejected** (workstream binding is immutable per session).
- **If a workstream is closed**, new sessions may **not** bind to it. Existing sessions in that workstream remain valid and may accumulate turns (though the closed workstream will not appear in the switcher).

### 4.2 Active workstream as ambient default target

When a client dispatches new work (via `task.run` or `session.create`) **without an explicit workstream binding**, the work targets the **active workstream** as the default (if one is active).

**Rules:**
- If the active workstream is `active` and a `task.run` arrives without `workstream_id`, the task implicitly binds to the active workstream.
- If no workstream is active, the task/session is **projectless** (valid).
- If the active workstream is later closed, existing sessions remain valid; new work defaults to the next active workstream (if any are switched to) or becomes projectless.

This feature is **implemented but wiring is deferred** (Phase 1B). The RPC layer supports it; the CLI/UI integration is Phase B+.

### 4.3 Workstream activation and deactivation

A client explicitly activates a workstream via `workstream.activate{id, force?}`:

- **`activate{id, force: false}` (default):** If no other workstream is active, activate this one and set state to `active` (assuming at least one session is live or the workstream is not closed). If another workstream is active, return `ActiveConflict{active_id}` (fail).
- **`activate{id, force: true}`:** Deactivate the current active workstream (if any) and activate this one. The prior workstream becomes `idle`. This is an **explicit switch**, never silent (consistent with DOC-40's "never silently multiplex" principle).

A client explicitly deactivates the active workstream via `workstream.deactivate{id}`:

- Only the active workstream may be deactivated. Deactivating an `idle` or `closed` workstream is a no-op (idempotent).
- After deactivation, `active_workstream_id` is set to `null`. The workstream transitions to `idle` (if it has live sessions).

### 4.4 Workstream closure

A workstream can be explicitly closed via `workstream.close{id}` (see §5.1). Closure is **irreversible**:

- The closed workstream **stops accepting new sessions** (turns attempting to bind are rejected).
- Existing sessions in the workstream **remain valid** (they may accumulate turns).
- If the closed workstream is the active one, it is automatically deactivated (sets `active_workstream_id` to `null`).
- Closed workstreams are **optionally listable** via a `include_closed: true` flag on `workstream.list` (for audit/historical query; default is false).

---

## 5. RPC/REST/CLI surface {#SPEC-WS-05~draft}

**ID:** SPEC-WS-05~draft
**Status:** Draft

### 5.1 JSON-RPC verbs (via `POST /rpc`)

The daemon adds the following JSON-RPC methods (to `crates/trusty-code/src/serve/http.rs` route table):

| Method | Signature (pseudocode) | Purpose |
|---|---|---|
| `workstream.create` | `{name?: String} → {id: UUID}` | Create a new workstream in the daemon's project. Name is initially empty (or user-provided). Returns the workstream ID. No `project_id` parameter — the daemon's own project is implicit. |
| `workstream.get` | `{id: UUID} → Workstream` | Fetch a single workstream record (full fields, inferred state). |
| `workstream.list` | `{include_closed?: bool} → Vec<Workstream>` | List all workstreams in the daemon's project. Default `include_closed = false`. Returns workstreams with inferred state, including which one (if any) is active. |
| `workstream.activate` | `{id: UUID, force?: bool} → {active_id: UUID, prior_id?: UUID}` | Activate this workstream (make it the active workstream). Default `force = false`. If another workstream is active, return `ActiveConflict{active_id}` (fail) unless `force: true`. When `force: true`, deactivates the prior workstream. Returns the new active ID and the prior ID (if any). This is an **explicit switch**, never silent (DOC-40 principle). |
| `workstream.deactivate` | `{id: UUID} → {}` | Deactivate the active workstream. Only the active workstream may be deactivated; deactivating an `idle` workstream is idempotent. Sets `active_workstream_id` to `null`. |
| `workstream.close` | `{id: UUID} → {}` | Irreversibly close a workstream. Rejects new session bindings; existing sessions remain valid. If the closed workstream is the active one, it is automatically deactivated. |
| `workstream.rename` | `{id: UUID, name: String} → Workstream` | Rename a workstream. Shipped in Phase C (issue #3300) — was deferred from Phase 1A. |

### 5.2 REST endpoints (wrappers around JSON-RPC)

Per #2983 Slice 4, REST alternatives are provided alongside JSON-RPC for one-shot operations:

| Method | Path | Purpose |
|---|---|---|
| `POST` | `/workstreams` | → `workstream.create{name?}` |
| `GET` | `/workstreams/{id}` | → `workstream.get{id}` |
| `GET` | `/workstreams?include_closed=…` | → `workstream.list{…}` |
| `POST` | `/workstreams/{id}/activate` | → `workstream.activate{id, force?}` |
| `POST` | `/workstreams/{id}/deactivate` | → `workstream.deactivate{id}` |
| `POST` | `/workstreams/{id}/rename` | → `workstream.rename{id, name}` (issue #3300, Phase C) |
| `POST` | `/workstreams/{id}/close` | → `workstream.close{id}` |

All endpoints return JSON. Errors are HTTP 4xx/5xx with a JSON error body (matching existing tcode error conventions).

**Path convention:** REST groups in trusty-code mount unprefixed (e.g., `/workstreams/`, `/sessions/`, `/tasks/`, `/fs/`) per the crate's existing convention; there is no `/api/v1` versioned prefix.

**Error codes:**
- `ActiveConflict{active_id}` (HTTP 409): returned by `activate{force: false}` when another workstream is active. Includes the ID of the currently-active workstream so the client can decide whether to retry with `force: true`.

### 5.3 Server-Sent Events (SSE) — multi-client observation and workstream-level aggregation

Clients (CLI, GUI, or agent) observe a workstream via a **workstream-level SSE endpoint** that aggregates events from all sessions bound to that workstream:

```
GET /workstreams/{id}/events
```

**Workstream event aggregation (key architecture):** The workstream-level endpoint is NOT a direct session-registry lookup. Instead, the daemon **fan-outs over the workstream's bound `session_ids`** internally:

1. On subscription to `/workstreams/{id}/events`, the daemon identifies all sessions in `workstreams[id].session_ids`.
2. The daemon internally subscribes to the event streams for each session (via the per-session SSE subscriber registry, which IS session-keyed per AC-7.3).
3. Events from each session are tagged with `{session_id, event_type, payload}` and forwarded to the workstream-level subscriber.
4. When sessions are added to the workstream (§4.1), new sessions are dynamically added to the fan-out.
5. When the workstream is deactivated (§4.2), clients reconnect to the new active workstream's `/workstreams/{new_id}/events` endpoint.

**Multi-client observation:** Multiple clients (CLI, GUI, agent) may observe the same workstream's events simultaneously (like `tmux attach-session` — any number of terminal clients can attach to one session). No per-client exclusive leases; the fan-out is daemon-managed.

**Harness-agnostic shared transport:** This aggregation layer (workstream event fan-out + per-session event tagging) is part of the **shared multi-client attach transport component** (§5.3.1/AC-7). It must be extracted to `trusty-agents-common` in Phase 1B so `trusty-agents` background sessions can reuse it for epic #3052 (iOS thin client over VPN).

**Event types (extensible; Phase 1A minimal set):**

| Event | Payload | Meaning |
|---|---|---|
| `SessionAdded` | `{session_id, binding_time}` | A session was bound to this workstream. |
| `SessionActivityUpdate` | `{session_id, last_turn_at, has_running_task}` | Activity status of a bound session (for state inference). |
| `WorkstreamStateInferred` | `{workstream_id, state: active \| idle \| closed, reason: String}` | Workstream state changed (computed). Fired when workstream is activated/deactivated/transitioned. |
| `WorkstreamActivationChanged` | `{new_active_id: UUID, prior_id?: UUID}` | A different workstream is now active (another client switched via `activate{force: true}`). Clients should reconnect to the new active workstream's events. |

### 5.3.1 Shared multi-client attach transport (harness-agnostic)

**Strategic design goal:** The multi-client SSE fan-out and reconnect/replay semantics used here (tmux-multi-attach-like) are **harness-agnostic and must be extracted to a shared component** in `trusty-agents-common` to serve both trusty-code workstreams and trusty-agents background sessions (epic #3052 — iOS thin client over VPN + live SSE). This follows the workspace-wide principle: **common entry point for any shared capability**.

**Phase 1A scope:** tcode may implement this as a tcode-private component initially. **Phase 1B scope (explicit AC below):** the interface must be designed against a session-id abstraction (a trait, not concrete `trusty_code::Session` types) so `trusty-agents` and future consumers can adopt it without rework. The extraction to `trusty-agents-common` is a mandatory follow-up (Phase 1B+), not deferred to Phase C.

**Harness-agnostic interface requirements:**
- **Session ID abstraction:** The per-session SSE endpoint accepts a generic `session_id` (UUID or string), not tcode-specific types. Subscribers fanout by session ID.
- **Event envelope:** Events are transported as a generic envelope: `{session_id, event_type, payload}`, allowing any harness to layer domain-specific events.
- **Reconnect/replay:** SSE ring-buffer replays N turns on reconnect (consistent with tcode session events §5.0); the mechanism is harness-agnostic.
- **Multi-subscriber registry (session-scoped):** A daemon-global subscriber registry (not per-workstream, but per-session) tracks active SSE connections by session ID, enabling fan-out and activation-change notifications (AC-7.3).
- **Logical-unit aggregation layer (workstream-level):** On top of the per-session registry, a workstream-level event aggregation layer fan-outs over `session_ids` and tags events per session. This allows a workstream to be observed as a single logical unit (§5.3) while maintaining session-scoped subscriber tracking underneath. The aggregation layer is also part of this harness-agnostic interface and must be extracted to `trusty-agents-common` (e.g., for epic #3052 where a background agent may manage multiple sub-sessions).

The trait/interface + aggregation layer **must live in `trusty-agents-common`** (alongside the workstream ledger, DOC-44); both tcode and tagents implement and consume it. Cross-ref DOC-44 and epic #3052 for adoption.

### 5.4 CLI surface — `tcode workstream` family

New CLI commands (in `crates/trusty-code/src/cli/`):

```bash
# List workstreams in the daemon's project
tcode workstream list [--include-closed]
tcode ws list [--include-closed]    # short alias

# Show a single workstream
tcode workstream get <id>
tcode ws get <id>

# Create a new workstream
tcode workstream create [--name=<name>]
tcode ws create [--name=<name>]

# Activate a workstream (switch to it)
tcode workstream activate <id> [--force]
tcode ws activate <id> [--force]

# Deactivate the active workstream
tcode workstream deactivate
tcode ws deactivate

# Close a workstream
tcode workstream close <id>
tcode ws close <id>
```

Output format: human-readable table (for list) or JSON (for get). The `tcode ws` short form rhymes with `tm ls` (tcode uses `workstream` as the full term; `ws` is the short alias).

When a CLI client runs `tcode ws activate`, it switches the active workstream and the connected CLI automatically starts streaming events from the new active workstream.

---

## 6. Activation-lock exclusivity model {#SPEC-WS-06~draft}

**ID:** SPEC-WS-06~draft
**Status:** Draft

### 6.1 Singleton active workstream per daemon

**At any instant, at most one workstream is `active` per daemon.** This is a daemon-enforced invariant:

- **`activate{id, force: false}` (default):** If no other workstream is active, activate this one. If another workstream is active, return `ActiveConflict{active_id}` (HTTP 409). The error includes the ID of the currently-active workstream, allowing the client to decide whether to retry with `force: true` or switch explicitly.
- **`activate{id, force: true}`:** Deactivate the currently-active workstream (if any) and activate this one. This is an **explicit switch**, never silent. The prior workstream transitions to `idle`. Returns the new active ID and the prior ID.
- **`deactivate{id}`:** Deactivate the active workstream. Only the active workstream may be deactivated; deactivating an `idle` workstream is idempotent.

### 6.2 Multi-client observation (unlike per-agent leases)

**Multiple clients (CLI, GUI, agent) may observe and drive the active workstream simultaneously.** This is the key difference from DOC-40's per-agent `AttachmentLease` model:

- **DOC-40 (per-agent leases):** One client holds an exclusive lease; others cannot observe or drive the agent until they take-over (eviction).
- **This spec (workstream activation):** Any number of clients can observe the active workstream via the multi-subscriber SSE endpoint. Clients do not need exclusive leases. Switching the active workstream is a daemon-enforced singleton gate, not a per-client gate.

This is **tmux-attach-like behavior**: any number of terminal clients can attach to one `tmux-session` simultaneously and drive it together. The "which tmux session is active" is a per-daemon question (not per-client).

### 6.3 Explicit switch — never silent (DOC-40 principle adopted at daemon level)

When a client calls `workstream.activate{id, force: true}` and another workstream is active, the activation is **explicit and visible**:

- All connected clients receive a `WorkstreamActivationChanged` event (§5.3).
- The prior workstream transitions from `active` to `idle`.
- The new workstream becomes `active`.
- Clients that were observing the prior workstream should reconnect to the new active workstream's events (the SSE endpoint path changes from `/workstreams/{old_id}/events` to `/workstreams/{new_id}/events`).

This preserves DOC-40's "never silently multiplex" principle at the activation level: when the active workstream changes, all clients are informed explicitly, never silently.

---

## 7. Acceptance Criteria {#SPEC-WS-07~draft}

**ID:** `SPEC-WS-07~draft`
**Status:** Draft

### AC-1: Core domain and persistence

**AC-1.1** A workstream record has the fields in §2.1: id, name, session_ids, created_at, updated_at, metadata. State is inferred, not persisted.

**AC-1.2** Workstreams are persisted to `~/.trusty-code/workstreams-{project_slug}-{hash}.json` as a single flat JSON file (not file-per-record) with a `workstreams` array and an `active_workstream_id` pointer.

**AC-1.3** A session may bind to a workstream at creation via `session.create(workstream_id)`. If a session with an existing workstream binding receives a task with a different workstream, the task is rejected.

**AC-1.4** On daemon restart, the workstream records and `active_workstream_id` are loaded and restored. The active workstream is re-activated (if it still exists).

### AC-2: RPC surface

**AC-2.1** All methods in §5.1 exist and are callable via `POST /rpc` (JSON-RPC): `create`, `get`, `list`, `activate`, `deactivate`, `close`.

**AC-2.2** All methods in §5.2 exist and are callable via REST (HTTP GET/POST).

**AC-2.3** `workstream.list` returns an array of workstreams with inferred state. Includes an `active_workstream_id` in the response (or `null` if none active).

**AC-2.4** `workstream.activate{force: false}` fails with `ActiveConflict{active_id}` if another workstream is active.

### AC-3: Activation-lock exclusivity (daemon-enforced singleton)

**AC-3.1** At most one workstream is `active` per daemon at any instant. This is a hard daemon invariant.

**AC-3.2** `workstream.activate{id, force: true}` deactivates the currently-active workstream and activates the specified one. Returns both the new and prior workstream IDs.

**AC-3.3** All connected clients receive a `WorkstreamActivationChanged` event when the active workstream changes (§5.3).

**AC-3.4** Multiple clients can observe the active workstream simultaneously via multi-subscriber SSE (no per-client exclusive leases).

### AC-4: State inference

**AC-4.1** Workstream state is inferred based on session activity (§2.2) and is never settable by the operator.

**AC-4.2** Only one workstream's state is `active` at a time. All others are `idle` or `closed`.

### AC-5: CLI

**AC-5.1** `tcode workstream list` works and returns human-readable output showing which workstream (if any) is active.

**AC-5.2** `tcode workstream activate <id>` switches to that workstream.

**AC-5.3** `tcode ws` short alias works for all `workstream` commands.

### AC-6: Non-destructive boot reconciliation

**AC-6.1** No workstream records are deleted during boot reconciliation (§3.3).

**AC-6.2** The active workstream is restored from the persisted `active_workstream_id` pointer (§3.2).

### AC-7: Harness-agnostic multi-client attach transport

**AC-7.1** The SSE endpoint and multi-subscriber fan-out use a session-ID abstraction (not tcode-specific types). The interface is defined as a trait or abstract type that `trusty-agents` can implement without rework (§5.3.1).

**AC-7.2** The endpoint accepts a generic `session_id` parameter (UUID or string) and returns an event envelope with `{session_id, event_type, payload}`. Domain-specific event payloads are layered on top.

**AC-7.3** The multi-subscriber registry (tracking active SSE connections) is daemon-global and session-scoped, not workstream-scoped. This allows reuse for tagents and other future harnesses (epic #3052).

**AC-7.4** A follow-up AC (Phase 1B): Extract the multi-client attach transport interface to `trusty-agents-common` and update both tcode and tagents to consume the shared component.

---

## 8. Phasing {#SPEC-WS-08~draft}

**ID:** SPEC-WS-08~draft
**Status:** Draft

### Phase 1A — Domain + persistence + RPC + CLI

- [x] Workstream domain model (§2)
- [x] Storage layout and reconciliation (§3, single flat workstreams.json)
- [x] RPC verbs and REST endpoints (§5, activation-lock model)
- [x] CLI commands (§5.4)
- [x] Activation-lock exclusivity (§6, singleton active workstream)
- [ ] GUI workstream switcher (deferred to Phase C, blocked on issue #3153 shell rebuild)

### Phase 1B (follow-up, not in this spec) — Session binding UI + ambient default

- Session-creation UI wiring to bind to workstream (§4.1)
- Ambient default-target logic: new work targets the active workstream (§4.2)

### Phase C (issue #3300) — GUI workstream switcher

- [x] Visual workstream switcher in the header (DOC-39 §2, principle 3)
- [x] State display, rename, close actions
- [x] Activation UI (switch between workstreams), including the
      `ActiveConflict` (409) surfacing + refresh affordance
- [x] `workstream.rename` RPC/REST verb (previously deferred, now shipped
      alongside its first caller)

---

## 9. Open questions for Bob {#SPEC-WS-09~draft}

**ID:** SPEC-WS-09~draft
**Status:** Draft

1. **Q1 — Workstream name inference:** When a session binds to a workstream, should we infer the workstream name from the first turn's prompt text (like DOC-39 suggests), or require the operator to name it at creation? Phase 1A can defer this to Phase 1B (name remains empty or "Untitled" until first session).

2. **Q2 — Project-ID storage namespace (PARTIALLY RESOLVED):** The storage filename `workstreams-{project_slug}-{hash}.json` is derived from the daemon's `ProjectBinding` (the project path or URL the daemon was launched for). The spec now states this explicitly (§3.1 notes "This naming scheme mirrors precedent"). Any remaining ambiguity: should the `project_slug` be derived from the path's basename (e.g., `acme-api` from `/path/to/acme-api`), or from a user-friendly project name if one is registered? Current spec assumes basename; implementation may refine.

3. **Q3 — Idle-to-active timeout:** How long should a workstream remain in `idle` state before it's eligible to be re-activated silently (or should it never auto-transition)? Current spec does not auto-transition; state is computed on each list/get call. CLI and GUI layers may auto-refresh.

---

## 10. Non-goals

- **Not a GUI switchboard (yet).** The workstream switcher visual design (DOC-39 §2) is out of scope. Phase C depends on issue #3153 (shell rebuild).
- **Not cross-machine discovery.** Workstreams are local to one user's `~/.trusty-code/` directory. No multi-machine or team collaboration.
- **Not multi-project sessions.** A workstream (per daemon) binds to the daemon's own project. No session may span projects.
- **Not a replacement for per-agent leases (DOC-40).** Workstreams use daemon-enforced singleton activation (§6), not per-client `AttachmentLease` semantics. Per-agent leases and background-agent attachment remain independent (DOC-40's model is unchanged).

---

## 11. Relationship to other specs

| Spec | Relationship |
|---|---|
| DOC-39 (trusty-code Harness UI) | This spec implements [`SPEC-TCUI-08~draft`](./trusty-code-harness-ui.md#SPEC-TCUI-08~draft) §4B (Workstream durability — a prerequisite). DOC-39 defines the domain object; this spec implements its persistence and lifecycle. |
| DOC-40 (Durable Background Agents) | This spec adopts [`SPEC-BGATTACH-04/05~draft`](./durable-background-agents.md#SPEC-BGATTACH-04~draft) "never silently multiplex" principle at the daemon level (§6.3): when the active workstream changes, all clients are informed explicitly. Per-agent leases (DOC-40) remain independent and unchanged. |
| DOC-44 (Engineering Lead Twin Orchestration, merged code) | DOC-44's `trusty_agents_common::workstreams::Workstream` (PR #3260) is the cross-tool ledger entry. This spec defines tcode's native `trusty_code::workstreams::Workstream` domain object. **Disambiguation:** code must use qualified imports to distinguish them. DOC-44 ledger entries reference tcode workstream IDs; they are complementary, not redundant. |
| DOC-38 (Spec-Linked Documentation) | This spec follows [`SPEC-SLD-02~draft`](./spec-linked-documentation.md#SPEC-SLD-02~draft) reference grammar, header block, anchoring, and catalog conventions. |

---

## 12. Verification and curl examples

### Setup

Assume a tcode daemon is running on `localhost:7882` (default).

### Create a workstream

```bash
curl -X POST http://localhost:7882/rpc \
  -H "Content-Type: application/json" \
  -d '{
    "jsonrpc": "2.0",
    "id": 1,
    "method": "workstream.create",
    "params": {"name": "Token rotation hardening"}
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
curl -X POST http://localhost:7882/rpc \
  -H "Content-Type: application/json" \
  -d '{
    "jsonrpc": "2.0",
    "id": 2,
    "method": "workstream.list",
    "params": {}
  }'

# Response:
# {
#   "jsonrpc": "2.0",
#   "id": 2,
#   "result": {
#     "active_workstream_id": null,
#     "workstreams": [
#       {
#         "id": "a1b2c3d4-e5f6-4748-9a0b-1c2d3e4f5a6b",
#         "name": "Token rotation hardening",
#         "state": "idle",
#         "session_ids": [],
#         "created_at": "2026-07-19T14:32:00Z",
#         "updated_at": "2026-07-19T14:32:00Z"
#       }
#     ]
#   }
# }
```

### Activate a workstream

```bash
curl -X POST http://localhost:7882/rpc \
  -H "Content-Type: application/json" \
  -d '{
    "jsonrpc": "2.0",
    "id": 3,
    "method": "workstream.activate",
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
#     "active_id": "a1b2c3d4-e5f6-4748-9a0b-1c2d3e4f5a6b",
#     "prior_id": null
#   }
# }
```

### Switch to a different workstream

```bash
curl -X POST http://localhost:7882/rpc \
  -H "Content-Type: application/json" \
  -d '{
    "jsonrpc": "2.0",
    "id": 4,
    "method": "workstream.activate",
    "params": {
      "id": "b2c3d4e5-f6a1-4748-9a0b-1c2d3e4f5a6b",
      "force": true
    }
  }'

# Response (if another workstream was active):
# {
#   "jsonrpc": "2.0",
#   "id": 4,
#   "result": {
#     "active_id": "b2c3d4e5-f6a1-4748-9a0b-1c2d3e4f5a6b",
#     "prior_id": "a1b2c3d4-e5f6-4748-9a0b-1c2d3e4f5a6b"
#   }
# }
```

---

## 13. Implementation checklist (for code reviewer)

**Rev 2 contract:**
- [ ] Workstream storage: single flat `workstreams-{project_slug}-{hash}.json` file (atomic temp+rename), with `version`, `active_workstream_id`, and `workstreams[]` array.
- [ ] Boot reconciliation: loads all records, restores `active_workstream_id`, clears no stale data (persistence is deterministic, no TTL-based expiry).
- [ ] RPC methods implemented: `workstream.create`, `workstream.get`, `workstream.list`, `workstream.activate`, `workstream.deactivate`, `workstream.close`, `workstream.rename` (Phase C, issue #3300; NO attach/detach/heartbeat).
- [ ] REST endpoints wrap the RPC methods (POST /workstreams, GET /workstreams/{id}, POST /workstreams/{id}/activate, etc.).
- [ ] **NEW route**: `GET /workstreams/{id}/events` — workstream-level SSE endpoint that fan-outs over bound session_ids with event tagging `{session_id, event_type, payload}`.
- [ ] CLI commands: `tcode workstream list/get/create/activate/deactivate/close` (NO attach/detach; ws short alias works).
- [ ] State inference: workstream state is `active` IFF its id == `active_workstream_id` (not computed from session activity). Otherwise `idle` or `closed`.
- [ ] Activation-lock exclusivity: `activate{force: false}` returns `ActiveConflict{active_id}` if another workstream is active. `activate{force: true}` deactivates prior and activates new.
- [ ] SSE events: `SessionAdded`, `SessionActivityUpdate`, `WorkstreamStateInferred`, `WorkstreamActivationChanged` (NO LeaseRevoked).
- [ ] Session binding: optional at `session.create(workstream_id?)` and immutable thereafter.
- [ ] Project scoping: all workstreams in daemon are project-scoped (daemon-local via ProjectBinding; no cross-project sessions).
- [ ] AC-7: harness-agnostic transport interface is designed (generic session-id abstraction, event envelope, aggregation layer for multi-session logical units).
- [ ] All ACs in §7 pass.
- [ ] All curl examples in §12 succeed.
- [ ] **Grep check:** No mentions of `lease`, `heartbeat`, `TTL`, `.attach`, `.detach` except DOC-40 cross-references (verify each hit is intentional).

