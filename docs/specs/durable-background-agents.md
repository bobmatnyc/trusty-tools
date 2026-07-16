# DOC-40 — Durable Background Agents: Exclusive Attach/Detach Semantics

**Status:** Draft
**Subsystem:** trusty-mpm — daemon / session-manager / agent delegation; trusty-code — session registry / task executor (cross-crate)
**Owner:** Engineering (trusty-mpm + trusty-code)
**Last-updated:** 2026-07-16
**Spec ID:** `SPEC-BGATTACH-01~draft` … `SPEC-BGATTACH-07~draft` (DOC-40)
**Requested by:** Bob (repo owner) — *"ability of background agents to be durable, for user to attach to
them, and for PMs to re-attach or attach to new ones (but only one attachment at a time for a
sub-agent)."*
**Builds on:**
- [DOC-39 — trusty-code Harness UI](./trusty-code-harness-ui.md) (PR #2855, **unmerged** — claims
  DOC-39; this spec claims the next free number, **DOC-40**, per its own scan-before-claim note).
  §2.1 `SPEC-TCUI-09~draft` — **"the UI communicates with the daemon; the daemon provides all
  functionality"** — is the architectural frame this spec applies one layer down: the daemon is
  the sole durable home for a background agent, and *attach* is a client operation over the daemon
  API, never a UI-side or in-process capability.
- [`docs/trusty-code/vision-and-architecture-spec.md`](../trusty-code/vision-and-architecture-spec.md)
  — Axiom 4 (§, "The Daemon OWNS Sessions; CLI Attaches Over the API") already specifies a
  **session**-level `session.attach`/`session.detach` protocol and explicitly defers durability:
  §12 ("Session Durability Model") states *"session state is NOT persisted across daemon
  restarts... Phase 2+ may add session durability, but this is explicitly out of scope for P1A."*
  This spec **is** that Phase 2+ work, scoped to **background agents** (sub-agent delegations, one
  level below the session), not the PM session object §12 was written against.
- [DOC-14 — Session Manager (SM) Agent](./session-manager-agent.md) — trusty-mpm's daemon-owned
  session control surface (`daemon/api.rs`, `daemon/services/session_service.rs`), the concrete
  precedent for "the daemon, not the client process, owns lifecycle."
- Epic **#2343** (CLOSED, "Infinite Sessions") — its own scope line reads *"delegated sub-agent
  loops stay bounded/ephemeral"*. This spec **deliberately amends that scope decision**: it is the
  reason background agents are ephemeral today, and Bob's request is to reverse it.

**Cross-ref:** trusty-mpm's `session_proxy_*` MCP tools (`session_proxy_focus/message/summary/unfocus`)
are examined in §3.4 as the nearest existing "attach-like" mechanism and found **not** to satisfy
this spec's requirements (no exclusivity, no durability, no lease). tmux's native attach semantics
(§8.1) are examined as prior art for *why* exclusivity must be enforced.

> **Scope note.** This is a **functional spec**, not an implementation plan. It states what the
> product must do — domain objects, state transitions, the API surface, and acceptance criteria —
> without prescribing the exact Rust types. Per the binding layer-priority rule (API → CLI → TUI),
> §6 (API surface) is the normative core; everything else is downstream of it. The PR carrying this
> doc opens **no** Rust changes.

---

## 0. Terminology

- **PM (project manager) session** — the long-running orchestrating conversation (a trusty-mpm
  managed session backed by tmux + Claude Code today, or a trusty-code daemon session) that
  delegates work.
- **Background agent** — a unit of delegated work (a sub-agent run) dispatched by a PM that this
  spec makes **durable**: it must be able to keep running, and remain attachable, independently of
  whether the PM process that spawned it is alive, paused, or has exited.
- **Attach** — a client (user or PM) establishing the **single** live, interactive channel to a
  background agent: receiving its event/output stream and being able to steer it (send input,
  interrupt, cancel).
- **Detach** — a client voluntarily releasing that channel. The background agent keeps running.
- **Take-over** — a client acquiring the attach slot while another client currently holds it,
  explicitly evicting the prior holder.
- **Lease** — the time-bounded record of who currently holds the attach slot, renewed by heartbeat,
  that expires if its holder goes silent.

---

## 1. Motivation and problem statement {#SPEC-BGATTACH-01~draft}

**ID:** SPEC-BGATTACH-01~draft
**Status:** Draft

### 1.1 The concrete failure

A PM session dispatched a fix pass to a background agent. The PM session **paused** (a normal,
supported operation — see the `tm-session-pause` skill: it snapshots todos/git-state and prunes
worktrees). The background agent died with it. **The entire fix pass was lost** — not degraded,
not resumable, gone. There was no durable record of the agent's progress, no way to re-attach and
recover it, and no way for the user to have intervened even if they had noticed in time.

This is not a bug in one function; it is the **expected consequence of today's architecture** in
both crates this spec spans. Root-caused below.

### 1.2 Root cause — trusty-mpm: `agent_delegate` is bookkeeping, not a spawner

`daemon/mcp_backend.rs:137-176` — `agent_delegate` validates the calling session, checks the
per-agent circuit breaker (`breaker.allows_delegation()`), and records a
`Delegation{id, session_id, agent, tier, task}`. **That is the entire implementation.** Confirmed
by the test name at `mcp/tools/mod.rs:217`:
`agent_delegate_description_clarifies_it_does_not_execute`. The actual sub-agent work happens
**client-side, inside the PM's own Claude Code process**, via Anthropic's Agent/Task tool —
invisible to and unmanaged by the trusty-mpm daemon. `agent_delegate` only gates and logs the
*intent*; it never provisions anything the daemon itself owns.

Contrast this with trusty-mpm's **session**-level durability, which is real and already proven:
`session_manager/record.rs` persists a `SessionRecord` (id, tmux target, state, workspace path) to
`~/.trusty-mpm/session-manager/sessions.json` (`session_manager/store.rs:1-8`), with a boot-time
`reconcile_on_boot` (`session_manager/reconcile.rs:41-211`) that cross-references live tmux against
the store. **A trusty-mpm session survives its own PM process dying — a background agent does
not**, because that durability machinery was built one level up from where Bob is asking for it.

### 1.3 Root cause — trusty-code: sub-agent delegation is nested, synchronous, and shares the PM's cancel flag

`task::executor::spawn_task_run` (`executor.rs:118-132`) reserves the session's single execution
slot and `tokio::spawn`s **exactly one** task for the whole PM run (`run_and_record`,
`executor.rs:164`). Inside that one task, `delegate_to_agent` (`tools/delegate.rs:206-256`)
directly `.await`s `InProcessAgentRunner::run` (`runner/in_process.rs:437`) — the sub-agent's
`AgentLoop` runs **nested inside the PM's own task**, sharing the same `Arc<AtomicBool>` cancel
flag (`executor.rs:216`) and the same `Execution.handle` (`registry.rs:120-123`). If the PM's
execution is cancelled or the session's `SessionRegistry` entry is dropped (daemon restart —
`registry.rs:8` states persistence is "Phase 2+, out of scope"; `serve/mod.rs:92` constructs a
fresh, empty registry on every boot), the sub-agent is cancelled or lost **with no independent
existence of its own**. There is no `AgentId` type in the crate at all (grep confirms zero hits) —
only a `String` agent-config name, not unique per invocation, and no durable record of "sub-agent X
is currently running" outside the live call stack.

**Both root causes reduce to the same gap**: a background agent has no daemon-owned identity or
lifecycle independent of the process/task that spawned it. This spec introduces that identity.

### 1.4 Why this is now a spec, not a patch

Epic **#2343** ("Infinite Sessions," closed) explicitly scoped delegated sub-agent loops as
*"bounded/ephemeral"* — a deliberate, owner-approved decision to keep the PM's own context durable
while treating sub-agents as disposable. That was the right call for *that* epic's success metric
(a 500-turn PM session with `compaction_events == 0`). Bob's request now **amends** that decision
for a different failure mode: sub-agent work must not be silently destroyed by an event (session
pause, daemon restart) that was never meant to be destructive. This spec does not reopen #2343's
PM-transcript durability work; it adds a sibling durability guarantee one level down, for
background agents.

---

## 2. Alignment with DOC-39's daemon-is-everything constraint

DOC-39 §2.1 (`SPEC-TCUI-09~draft`) states four corollaries (C-1..C-4) that this spec adopts
verbatim, substituting "background agent" for "UI":

- **A background agent's durable state lives in exactly one place: the daemon.** Not the spawning
  PM's process memory, not a client's local state, not (for trusty-mpm) an in-process Claude Code
  Task-tool invocation the daemon cannot see.
- **Attach is a daemon API call**, never a side-channel (e.g., "just re-run the same tmux
  attach-session command and hope the pane is still there" is not attach — see §8.1 on why tmux's
  own multi-client model is insufficient).
- **No capability divergence**: a CLI, a TUI, and (for trusty-mpm) a Telegram/TELUI client must all
  be able to attach identically, because they all call the same daemon endpoint (§6).
- **A UI/CLI need with no API is an unbuilt feature, not a client-side workaround** — this governs
  §6: every verb in §5 must exist as a daemon endpoint before any client surfaces it.

Where this spec **extends** DOC-39 rather than merely restating it: DOC-39 is scoped to
trusty-code's SPA. This spec is cross-crate — it applies the same constraint to trusty-mpm's
daemon, because that is where the reported failure actually occurred and where the durable-session
precedent (§1.2) already exists.

---

## 3. Domain model {#SPEC-BGATTACH-02~draft}

**ID:** SPEC-BGATTACH-02~draft
**Status:** Draft

| Object | Runtime status | Definition |
|---|---|---|
| **BackgroundAgent** | **NEW** | A daemon-owned record of one delegated unit of work: stable `agent_id`, spawning PM's identity, task description, current lifecycle state (`Spawned → Running → (Paused) → Done \| Failed \| Cancelled`), and a pointer to its durable output (transcript/event log). Exists **independently** of the spawning PM's process — decoupling is the whole point of §4. |
| **AttachmentLease** | **NEW** | The exclusivity record for one `BackgroundAgent`: `{holder_id, holder_kind: User \| Pm, acquired_at, lease_expires_at, connection_token}`. At most one **live** lease per background agent (§5). |
| **SessionRecord** (trusty-mpm) | **EXISTING — durable** | `session_manager/record.rs:138-320`. Proven pattern: FSM (`Provisioning → Active ⇄ Stopped/Errored → Decommissioned`, `record.rs:92-111`), JSON persistence, tmux-backed. `BackgroundAgent` reuses this shape rather than inventing a new one (§4.1). |
| **SessionEntry / SessionRegistry** (trusty-code) | **EXISTING — NOT durable** | `session/registry.rs:63-158`. In-memory only (`registry.rs:8`); ring-buffer + non-exclusive `attach`/`detach` (`registry.rs:384-425`) already exists at the *session* level and is the direct reusable pattern for `BackgroundAgent`'s event delivery (§6.2) — it just needs (a) durability and (b) exclusivity added, and (c) applied one level down, to sub-agents instead of only to the top-level session. |
| **Delegation record** (trusty-mpm) | **EXISTING — bookkeeping only** | `daemon/mcp_backend.rs:137-176`. `{id, session_id, agent, tier, task}`. **Superseded, not deleted**: `agent_delegate` becomes the trigger that provisions a `BackgroundAgent` (§6.1), and the existing circuit-breaker gate (`breaker.allows_delegation()`) is preserved unchanged. |
| **Focus map** (trusty-mpm `session_proxy_*`) | **EXISTING — insufficient** | `client/proxy.rs:341-344`. Per-conversation `HashMap<conversation_key, FocusTarget>`, in-memory, non-exclusive (any number of conversations may focus the same session), no lease/heartbeat (§3.4 detail). Not reused as-is; `AttachmentLease` is a purpose-built replacement for the background-agent case. |
| `AgentSpawned` / `AgentStarted` / `AgentDone` / `AgentFailed` (trusty-code events) | **EXISTING BUT DEAD** | `events.rs:193,312,202,207`. Defined in the `Event` enum with working `.kind()`/`.session_id()` match arms (`events.rs:368-430`), but grep confirms **zero construction sites** outside unit tests — `delegate_to_agent` today only ever emits generic `ToolStarted`/`ToolFinished` (`task/sink.rs:33-79`). Wiring these is the trusty-code-side Phase 1 item (§9). |

### 3.1 `AgentId` — the missing stable identifier

Neither crate has one today (§1.3). This spec requires a stable `AgentId` (UUID or similar),
**distinct from** the session that spawned it and **distinct from** the agent-config name (which is
a reusable string like `"python-engineer"`, not unique per invocation). `AgentId` is the primary
key for `BackgroundAgent` and `AttachmentLease` throughout this spec.

**AC-1.1** Every background agent has exactly one `AgentId`, minted at spawn time, stable for the
agent's entire lifetime including across PM-session death and daemon restart.
**AC-1.2** Two concurrent delegations to the same agent-config name produce two distinct
`AgentId`s and two independent `BackgroundAgent` records.

---

## 4. Durability model {#SPEC-BGATTACH-03~draft}

**ID:** SPEC-BGATTACH-03~draft
**Status:** Draft

### 4.1 Decoupling agent lifecycle from the spawning session

**Requirement.** A `BackgroundAgent`'s `JoinHandle`/task, cancellation flag, and durable record
MUST NOT be owned by, or nested inside, the spawning PM's own execution unit.

- **trusty-code today** (§1.3): sub-agent execution is nested inside the PM's single `Execution`,
  sharing its cancel flag and handle. **Required change**: promote sub-agent execution to its own
  `Execution`-shaped record (own `JoinHandle`, own cancel flag), held by the `SessionRegistry` (or
  a new daemon-global `AgentRegistry`) keyed by `AgentId`, not nested inside the PM's `Execution`.
  Cancelling/losing the PM's execution MUST NOT cancel a `BackgroundAgent` that has been marked
  durable (§4.4 distinguishes "durable" from "attached-only, ephemeral" agents — not every
  delegation needs to survive its parent; see §9 open question on default policy).
- **trusty-mpm today** (§1.2): delegated work runs invisibly inside the PM's own Claude Code
  process via the Task tool — the daemon has no handle to it at all. **Required change**:
  `agent_delegate` must provision something the daemon *itself* owns and can keep alive — in
  practice, the same durable pattern trusty-mpm already uses for whole sessions
  (`SessionRecord` + tmux backing, §4.2), applied to the delegated unit of work.

### 4.2 State persistence

`BackgroundAgent` reuses `SessionRecord`'s proven persistence shape (`record.rs:138-320`,
`session_manager/store.rs`) rather than inventing new storage:

- Persisted as JSON (or the daemon's existing session-store backend), keyed by `AgentId`, with
  fields: `agent_id`, `parent_session_id` (nullable after the parent is gone — see AC-2.3), `agent
  kind/config name`, `task description`, `state`, `created_at`/`last_activity_at`,
  `output_pointer` (transcript/event-log location), and — for trusty-mpm, where a background agent
  is realized as its own tmux-backed unit — a `tmux_name`/`pane_id` exactly as `SessionRecord`
  already carries (`record.rs:318`).
- **AC-2.1** A `BackgroundAgent` record is written to durable storage **before** `agent_delegate` /
  `delegate_to_agent` returns to the caller — not best-effort, not fire-and-forget (mirrors §1.2's
  observation that `agent_delegate` today returns having done nothing durable).
- **AC-2.2** A `BackgroundAgent`'s state and accumulated output survive a daemon restart.
- **AC-2.3** A `BackgroundAgent` outlives its spawning PM session. If the PM session is later
  decommissioned/deleted, the `BackgroundAgent` is NOT cascade-deleted — it becomes **orphaned**
  (still attachable, `parent_session_id` retained as historical metadata) rather than destroyed.
  This is the direct fix for §1.1's failure: pausing (or even terminating) the PM must not destroy
  the agent.

### 4.3 Reconciliation on daemon boot

Reuse the proven pattern at `session_manager/reconcile.rs:41-211`: on boot, cross-reference the
persisted `BackgroundAgent` set against actually-live execution state (tmux panes for trusty-mpm;
live `tokio` tasks — which do NOT survive a restart, so trusty-code's reconciliation is necessarily
state-based, not task-based — for trusty-code).

- **AC-3.1** On boot, a `BackgroundAgent` whose backing process/task is confirmed alive is restored
  to `Running`; one whose backing process is confirmed gone transitions to a terminal state
  (`Failed{reason: "daemon restart, no surviving process"}` for trusty-code's necessarily
  in-process model, or `Stopped`/resumable for trusty-mpm's tmux-backed model — see §9 open
  question on whether trusty-code needs an actual out-of-process execution model to make
  `BackgroundAgent`s survive a *daemon* restart, not just a PM-session death, symmetrically with
  trusty-mpm).
- **AC-3.2** Reconciliation never silently drops a record; every persisted `BackgroundAgent` has a
  determinate post-boot state, mirroring `reconcile.rs`'s "no orphaned state, only resumable or
  tombstoned" invariant (`record.rs:81-88`).

### 4.4 Not every delegation needs to be durable

**Design decision.** Making *all* delegations durable is unnecessary overhead for short-lived,
synchronous sub-agent calls (e.g., a one-shot lookup that returns in seconds). This spec's
durability guarantee applies to delegations explicitly marked **background** — the caller (PM)
opts in per-delegation (§6.1's `durable: bool` parameter), matching how `agent_delegate` already
distinguishes tiers via the circuit breaker. Default value is an open question (§10, Q1).

---

## 5. Attach protocol {#SPEC-BGATTACH-04~draft}

**ID:** SPEC-BGATTACH-04~draft
**Status:** Draft

### 5.1 Why exclusivity — tmux prior art and the interleaving hazard

trusty-mpm's actual tmux attach mechanism (`bin/tm/commands/tmux_attach.rs`) does **zero** locking:
outside tmux it shells to real `tmux attach-session -t <name>`; inside tmux it uses `switch-client
-c <client_tty> -t <name>` (two hardening rounds, #2678/#2680, were about correctly identifying
*which local client* to redirect — never about excluding other attachers). tmux's own default
behavior is **multi-client mirroring**: every attached client sees the same pane and every client's
keystrokes go to the same shell. No `-d` (force-detach) flag is used anywhere in the crate today.

**This spec deliberately does NOT inherit that default.** The reason is specific to *agents*, not
terminals: a human watching a shared shell and typing occasionally is a reasonable multi-client
experience. An **LLM agent mid-turn** is not a shell — it is accumulating context from a single
linear conversation. Two controllers sending input to the same agent turn — a PM re-attaching to
"check in" while a user is mid-steer, or two PM instances both believing they own the same
background agent — **interleaves two independent steering intents into one context window**. The
agent cannot distinguish "the user's correction" from "a stray PM nudge that arrived mid-thought";
the result is not a race condition in the infrastructure sense, it is **corrupted agent context**
that produces confused or contradictory output with no clean way to detect *after the fact* that it
happened. Exclusivity is therefore a **correctness requirement for the agent's context integrity**,
not a UX preference — this is the one place this spec explicitly departs from tmux's own multi-
client norm despite reusing tmux as a transport for trusty-mpm's background agents.

### 5.2 Verbs

| Verb | Effect |
|---|---|
| `acquire` | Attempts to take the attach slot for `agent_id`. Succeeds immediately if unattached. If attached, fails with `ALREADY_ATTACHED{holder_kind, acquired_at}` **unless** the caller passes `force: true` (→ `take-over`, see below). Success returns a `connection_token` and the current lease's `lease_expires_at`. |
| `heartbeat` | Renews an existing lease. Must be called by the holder before `lease_expires_at`, or the lease is considered abandoned (§5.4). |
| `release` | Voluntary detach. Frees the slot immediately; the background agent keeps running. |
| `take-over` | `acquire{force: true}`. Explicitly evicts the current holder (closes their event stream / sends them a `LeaseRevoked` notification if they are still connected) and installs the caller as the new holder. **Never silent** — see AC-4.4. |

### 5.3 Exclusivity enforcement

**AC-4.1** At most one **live** `AttachmentLease` exists per `BackgroundAgent` at any instant.
**AC-4.2** `acquire` without `force` on an already-attached agent returns a typed error, never
silently multiplexes the caller alongside the existing holder (this is the literal requirement
Bob stated: *"attach must fail or take-over explicitly, never silently multiplex"*).
**AC-4.3** A caller's own repeated `acquire` for a lease they already hold is idempotent (renews
rather than erroring) — this is what makes PM re-attach (§7.1) cheap to call defensively.
**AC-4.4** A `take-over` MUST deliver a `LeaseRevoked{new_holder_kind}` notification to the evicted
holder's still-open channel before closing it, if that channel is still live. If the prior holder
is already gone (dead lease, §5.4), there is nothing to notify — this is the expected common case
for PM re-attach after a session pause.

### 5.4 Heartbeat / lease timeout — a dead client must not hold the lock forever

**Requirement.** Directly addresses the brief's mandate: *"heartbeat/lease so a dead client doesn't
hold the lock forever."*

- Each `AttachmentLease` carries `lease_expires_at`, initially `acquired_at + LEASE_TTL` (default
  TTL is an open question, §10 Q2 — candidate default 60s, renewed by the transport's own
  keep-alive, e.g. SSE's existing connection liveness, so most clients never need an explicit
  heartbeat call).
- **AC-5.1** A lease whose `lease_expires_at` has passed with no renewal is **not** a live lease for
  the purposes of AC-4.1/4.2 — a subsequent plain `acquire` (no `force` needed) succeeds and treats
  the expired lease as vacated.
- **AC-5.2** Lease expiry does NOT affect the background agent's own execution state — an
  unattended, unleased agent keeps running exactly as it would while attached (this is the
  "durable" half of durable-attach: attachment is observation/steering, not a keep-alive for the
  work itself).
- **AC-5.3** Transport-level disconnect (SSE connection drop, tmux client detach) is treated as an
  immediate signal to shorten the remaining TTL, not an instant release — a brief network blip
  should not force a take-over storm, but should also not require waiting the full TTL if the
  disconnect is unambiguous (e.g., a clean SSE close). Exact grace-period behavior is
  implementation detail, not normative here.

### 5.5 Lease holder identity

`holder_kind` is `User | Pm`. This governs §7's two flows and lets a client render *who* currently
owns an agent (needed for take-over UX — you want to know you're evicting a person, not stale
infra) without this spec prescribing anything about authentication (out of scope — see §10 Q3,
mirroring DOC-39 §7 Q3's open auth question, which this spec inherits unresolved).

---

## 6. API surface {#SPEC-BGATTACH-05~draft}

**ID:** SPEC-BGATTACH-05~draft
**Status:** Draft

**Per the layer-priority rule (API → CLI → TUI) and DOC-39 §2.1's stronger daemon-is-everything
reading: every verb below MUST exist as a daemon endpoint before any CLI or TUI surfaces it, and a
CLI/TUI need with no endpoint is an unbuilt feature, never a client-side workaround.**

### 6.1 Bob's API-testable-locally principle (normative)

**Any proxy/channel binding must be API-testable locally before it is wired to a live channel.**
Concretely:

**AC-6.1** Every verb in §5.2 MUST be exercisable via plain HTTP (`curl`) or the daemon's local
JSON-RPC transport, with no dependency on tmux, Telegram, a TUI, or any other live channel.
**AC-6.2** A local integration test exercising the full sequence — `spawn (durable) → acquire →
heartbeat → take-over → release` — against a real daemon instance MUST exist and pass **before**
any channel-specific wiring (tmux attach, Telegram bot, TUI pane) lands for this feature. This is
the acceptance gate for Phase 1 (§9).

### 6.2 trusty-mpm daemon surface (primary target — §1.2's root cause lives here)

Extends `daemon/api.rs` / `daemon/services/session_service.rs` (the existing durable-session
control surface, DOC-14) and the MCP tool surface (`mcp/tools/session.rs`,
`mcp/session_dispatch.rs`) with a new agent-scoped family, sibling to the existing
`session_proxy_*` tools but purpose-built for exclusivity (§3, "Focus map" row — not reused):

| Endpoint / MCP tool | Purpose |
|---|---|
| `POST /api/v1/agents` (or `agent_delegate` extended with `durable: bool`) | Spawns a `BackgroundAgent`; returns `agent_id`. Supersedes bookkeeping-only `agent_delegate` per §4.2 AC-2.1. |
| `GET /api/v1/agents` / `agent_list` | Lists `BackgroundAgent`s, filterable by `parent_session_id`, state, orphaned-only. |
| `GET /api/v1/agents/{id}` / `agent_status` | Current `BackgroundAgent` state + current lease holder (if any). |
| `POST /api/v1/agents/{id}/acquire` / `agent_attach{agent_id, force}` | §5.2 `acquire`/`take-over`. |
| `POST /api/v1/agents/{id}/heartbeat` | §5.2 `heartbeat`. |
| `POST /api/v1/agents/{id}/release` / `agent_detach{agent_id}` | §5.2 `release`. |
| `GET /api/v1/agents/{id}/events` (SSE) | Event/output stream — **gated on holding the current lease**; a non-holder's `GET` is rejected (contrast with trusty-code's session-level SSE endpoint today, §6.3, which is intentionally unauthenticated and multi-subscriber — this endpoint is not). |

### 6.3 trusty-code daemon surface (sibling implementation, same protocol shape)

Extends `session/protocol.rs` (which already registers `session.attach`/`session.detach` at the
**session** level, non-exclusive by design for multi-viewer session monitoring) with a
**sub-agent**-scoped family that layers exclusivity on top of the existing reusable primitives
(ring-buffer replay + `crate::events::subscribe()` broadcast, `serve/http.rs:143-146`):

| Method | Purpose |
|---|---|
| `agent.spawn(session_id, agent_name, task, durable: bool)` → `AgentId` | Promotes `delegate_to_agent` from a nested synchronous call (§1.3) to an independent `Execution`-shaped record when `durable: true`. |
| `agent.list(session_id?)` | Lists background agents, optionally scoped to a session. |
| `agent.status(agent_id)` | Current state + lease holder. |
| `agent.acquire(agent_id, force?)` | §5.2. |
| `agent.heartbeat(agent_id)` | §5.2. |
| `agent.release(agent_id)` | §5.2. |
| `GET /agents/{id}/events` (SSE) | Lease-gated, mirroring §6.2's endpoint — **not** the existing unauthenticated `GET /sessions/{id}/events` pattern. |

This is also where trusty-code's already-defined-but-dead events (`AgentSpawned`/`AgentStarted`/
`AgentDone`/`AgentFailed`, `events.rs:193,312,202,207`) get their first real construction sites —
wiring them is a prerequisite, not a side effect (§9 Phase 1).

### 6.4 CLI (thin client, second priority)

```
tm agent list [--session <id>] [--orphaned]
tm agent status <agent-id>
tm agent attach <agent-id> [--force]        # acquire, then stream events, forwarding stdin
tm agent detach                              # release the currently-held lease
```

No orchestration logic in the CLI — every verb is a direct call to §6.2/§6.3.

### 6.5 TUI (third priority, out of scope for this spec's Phase 1)

A `tm sessions tui`-style pane (DOC-16 precedent) showing background agents grouped by
parent/orphaned, with an `a` keybinding for attach and a visible "held by: <kind>, expires in
<Ns>" indicator. Deferred — see §9.

---

## 7. PM re-attach and user attach flows {#SPEC-BGATTACH-06~draft}

**ID:** SPEC-BGATTACH-06~draft
**Status:** Draft

### 7.1 PM re-attach flow

1. A PM session resumes (via `tm-session-resume` or equivalent) after a pause/restart.
2. It calls `agent_list(parent_session_id: self)` (or the orphaned-agents variant if its own
   `session_id` changed across the pause — see §9 open question on session-id stability across
   pause/resume) to discover background agents it previously spawned that are still `Running` or
   reached a terminal state while it was away.
3. For each, it calls `acquire` (no `force`) to resume observing/steering. Per **AC-4.3**, if no
   other client attached in the interim, this succeeds immediately and cheaply — the PM does not
   need to know in advance whether anyone else is attached.
4. If a **user** attached in the interim (`ALREADY_ATTACHED{holder_kind: User}`), the PM does
   **not** silently take over — see §10 Q4 for whether the PM should ever auto-force here. Default
   behavior: surface the conflict to the operator rather than resolve it unilaterally, consistent
   with §5.1's rationale (a PM re-attach is exactly the kind of "stray nudge mid-thought" the
   exclusivity model exists to prevent).

### 7.2 User attach flow

1. User lists background agents (`tm agent list`, or the equivalent TUI/CLI surface) — including
   ones spawned by a PM session that has since paused or exited (**AC-2.3**: orphaning, not
   deletion, is what makes this possible at all).
2. User calls `attach <agent-id>`. If unattached, succeeds immediately. If a PM (or another user
   session) currently holds the lease, the user sees who holds it and its lease's remaining TTL,
   and may pass `--force` to take over (**AC-4.4**: the evicted holder, if still live, is notified,
   never silently dropped).
3. While attached, the user has the exclusive channel: sees the live event stream and can send
   input, exactly as if this were the PM's own conversation, with the guarantee that no second
   controller (including the spawning PM re-attaching without `force`) is concurrently steering the
   same agent (**AC-4.1**/**AC-4.2**).
4. User detaches (`release`) or the lease is auto-vacated on disconnect (**AC-5.3**); the agent
   keeps running either way (**AC-5.2**).

---

## 8. Failure modes {#SPEC-BGATTACH-07~draft}

**ID:** SPEC-BGATTACH-07~draft
**Status:** Draft

| Failure | Behavior |
|---|---|
| Attaching client crashes without releasing | Lease expires per TTL (§5.4); no permanent lock. |
| Two clients race `acquire` simultaneously | Daemon-side single-writer ordering (the existing `Mutex`/`RwLock` around the registry, e.g. `session/registry.rs`'s pattern) makes exactly one the winner; the loser gets `ALREADY_ATTACHED`, never a torn/partial lease. |
| Background agent itself crashes while attached | State transitions to `Failed`; the attached client's event stream receives a terminal event (reusing the existing `AgentFailed` event, wired per §6.3) rather than silently hanging; lease is released automatically since there is nothing left to steer. |
| Daemon restarts while an agent is attached | Lease is not durable across restart (in-memory by design — a lease is a live-connection concept, not a durable-state concept); the agent's own durable state **is** restored per §4.3, and the previously-attached client must re-`acquire` on reconnect — this is expected, not a bug, and matches AC-5.1's "expired lease vacates" semantics applied to the degenerate case of "the whole daemon, and therefore every lease, restarted." |
| `take-over` on an agent with no prior lease at all | Behaves identically to plain `acquire` — `force: true` is a no-op when there is nothing to evict, never an error. |
| Orphaned agent (parent session deleted) never gets attached by anyone | Runs to completion or its own terminal state per its own logic; §4 does not introduce a TTL on the agent itself, only on leases — agent-level idle-expiry is explicitly deferred (mirrors the sibling vision-and-architecture-spec §12's own "Phase 2+" idle-expiry note for sessions). |

---

## 9. Phased delivery

**Phase 1 (this spec's minimum viable slice):**

1. `AgentId` type in trusty-code; `agent_id`-keyed `BackgroundAgent` domain object in both crates
   (§3, §4.1).
2. Durable persistence + boot reconciliation (§4.2, §4.3) — reuse `SessionRecord`/`SessionStore`
   patterns in trusty-mpm; new (necessarily in-memory-restored, not process-restored) persistence
   in trusty-code per AC-3.1's caveat.
3. `acquire`/`heartbeat`/`release`/`take-over` with lease TTL (§5) as **daemon endpoints only** —
   HTTP/JSON-RPC, curl-testable, per **AC-6.1/6.2**. No CLI, no TUI, no tmux/Telegram wiring yet.
4. Wire trusty-code's dead `AgentSpawned`/`AgentStarted`/`AgentDone`/`AgentFailed` events (§6.3) —
   prerequisite for the lease-gated SSE endpoint to carry meaningful content.
5. The local integration test mandated by **AC-6.2**, as the Phase 1 exit gate.

**Explicitly NOT Phase 1:** CLI (`tm agent ...`), TUI surface (§6.5), tmux-backed realization of
`BackgroundAgent` for trusty-mpm (Phase 1 can validate the protocol against an in-process/HTTP-only
background agent before committing to a tmux-pane-per-agent implementation), Telegram/TELUI
attach, auto-force policy for PM re-attach (§10 Q4).

**Phase 2+:** CLI, TUI, tmux realization, channel wiring, cross-crate protocol unification (§10
Q5).

---

## 10. Open questions

**Q1 — Default durability.** Should `durable: bool` on a delegation default to `true` (every
background agent is durable unless opted out) or `false` (opt-in, matching #2343's existing
"ephemeral by default" posture)? Defaulting to `true` directly prevents recurrences of §1.1's
failure but changes the resource/persistence cost of every delegation. **Owner: Bob.**

**Q2 — Lease TTL value and heartbeat cadence.** §5.4 proposes a 60s default riding on transport
keep-alive. Is an explicit heartbeat call needed at all, or is "connection is still open" a
sufficient liveness signal for every transport this spec targets (HTTP/SSE, tmux, future TUI)?
**Owner: engineering.**

**Q3 — Auth / multi-user identity for `holder_kind`.** This spec assumes `User` vs `Pm` is
distinguishable but does not specify how (mirrors DOC-39 §7 Q3, unresolved there too). Single-
operator assumption inherited, not re-litigated. **Owner: Bob.**

**Q4 — Should a PM re-attach ever auto-force?** §7.1 defaults to "surface the conflict, never
auto-force." Is there a case (e.g., the PM detects the user's session has been idle past some
threshold) where auto-take-over is desirable, or does that reintroduce the interleaving hazard
§5.1 exists to prevent? **Owner: Bob.**

**Q5 — Cross-crate protocol unification.** trusty-mpm and trusty-code each get their own
implementation of §5's verbs (§6.2/§6.3) rather than a single shared daemon. Should these converge
on a literally shared library/protocol crate (e.g. inside `trusty-common`), given they are
structurally identical, or is duplication acceptable given the two daemons' different transports
(tmux vs in-process `tokio` tasks)? **Owner: engineering.**

**Q6 — Does trusty-code need out-of-process background-agent execution to survive a *daemon*
restart, not just a PM-session pause?** §4.3 AC-3.1 currently accepts that a trusty-code background
agent cannot survive the daemon process itself dying (only trusty-mpm's tmux-backed model can, by
construction). Is that asymmetry acceptable, or does full parity require trusty-code to spawn
background agents as separate OS processes? This is a substantially larger change than this spec's
Phase 1. **Owner: engineering / Bob.**

---

## 11. Follow-ups

| ID | Item | Depends on |
|---|---|---|
| **F1** | Add the DOC-40 catalog row to `docs/specs/README.md`, and re-scan for next-free `DOC-N` once PR #2855 (DOC-39) merges (its own note already anticipates this collision). | this spec |
| **F2** | File the Phase-1 issues (§9) — domain object + persistence + lease protocol + local integration test — sequenced ahead of any CLI/TUI/channel work. | this spec |
| **F3** | Resolve Q1 (default durability) and Q4 (auto-force policy) before Phase 1 implementation starts — both are behavior-affecting, not deferrable to Phase 2. | Bob |
| **F4** | Evaluate Q5 (shared protocol crate) once both crates have independent Phase 1 implementations to compare. | engineering |

---

## Changelog

- **2026-07-16** — Initial draft (DOC-40, `SPEC-BGATTACH-01~draft` … `SPEC-BGATTACH-07~draft`),
  requested by Bob. Root-causes today's data-loss failure in both trusty-mpm (`agent_delegate` is
  bookkeeping-only, §1.2) and trusty-code (nested synchronous sub-agent execution sharing the PM's
  cancel flag, §1.3); introduces `BackgroundAgent` + `AttachmentLease` as new durable domain
  objects; specifies the acquire/heartbeat/release/take-over protocol with mandatory exclusivity
  and lease-timeout semantics; aligns with DOC-39 §2.1's daemon-is-everything constraint and amends
  epic #2343's "sub-agent loops stay ephemeral" scope decision.
