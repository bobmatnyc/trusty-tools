# DOC-60 — Unified Agent Communication: User ↔ Assistant, Assistant ↔ Sub-Agent, Assistant ↔ Assistant

**Status:** Draft
**Spec ID:** `SPEC-AGENTBUS-01~draft`
**Subsystem:** trusty-mpm (bus host, daemon) — trusty-agents (assistants, sub-agents, ctrl) — trusty-channels (Slack/Telegram/etc.) — trusty-memory (consolidation target) — trusty-search (index target)
**Last-updated:** 2026-07-28 (Rev 4: owner-ratified decision that chat
consolidates onto this bus — turn-level user prompts, assistant final
responses, and peer messages are all canonical bus traffic, widening the
bus's role beyond the peer channel it was originally scoped as and bringing
§5.1 into scope for the bus path on equal footing with §5.3 (§1, §5.1); tool
calls are explicitly excluded and stay intra-turn execution-loop metadata on
the turn's response envelope, never independent bus traffic, for the same
request/response-shaped cost reasons that already keep token deltas off this
bus (§5.2, §11); the "all responses ride the bus" claim is reconciled
explicitly against "no deltas ride the bus" — one completed-turn envelope,
deltas over the legacy path only (§11); a read model / write model split is
established — the bus is the durable write path and `trusty-memory`'s
`chat_session` becomes a derived, read-side projection fed from the bus
rather than an independent writer, with dual-writing chat as two
independently-written durable records now explicitly prohibited (§9);
sequencing is recorded — chat-view rehydration ships first against
`chat_session` as written today, then the writer is re-pointed at the bus
once §5.1 publishing and the replay/thread query surface land, frontend
unchanged (§9); and a new open question on how a §5.1 user-edge publish is
authenticated against PR #4261's registered-instance model is added,
deliberately left unanswered (§12 Open Question 10). Rev 3: three owner
decisions recorded — (1) the
persona peer channel (§5.3) ships independently of ADR-0019, with the
persona-layer-only scope of the "one bus" claim made explicit (§1); (2)
instance-bypass addressing is in scope and normative in §5.3, with the
failure-mode semantics left pending the implementing engineer's decision
(§12 Open Question 8, narrowed accordingly); (3) streaming chat token deltas
explicitly stay on the retained per-crate buses (§3 #1/#2/#5) rather than
riding this bus, stated normatively in §11. Rev 2: §12 Open Question 9,
decline-ability of a peer request, moved into §5.3 as a normative consequence
derived from ADR-0024's virtual-twin authority principle; remaining open
questions renumbered. Rev 1: unified all three communication edges —
user↔assistant, assistant↔sub-agent, assistant↔assistant — into
equally-specified first-class sections per owner instruction; documents
assistant↔assistant as the replacement for the delegation lane closed by
PR #4240/ADR-0024)

> **Trusty Code disposition (2026-09-03):** This shared bus stays Draft and is
> not implemented by Trusty Code. Its `SessionEventEnvelope` remains the local
> ordered replay and live stream; it must not be presented as the durable
> cross-agent channel. Adoption is tracked by #5429 and governed by ADR-0058.

## 1. Summary

This spec unifies **three** communication paths onto **one** addressed,
durable, searchable message bus, as equally-specified first-class edges:

1. **User ↔ Assistant/PM** (§5.1)
2. **Assistant/PM ↔ Sub-Agent** (§5.2)
3. **Assistant ↔ Assistant, peer-to-peer** (§5.3)

Every cross-boundary persona message in the system — these three edges plus
inbound/outbound channel traffic (§8) — rides this one bus, retiring the five
independent broadcast implementations that exist today because none of them
address the unit that actually needs addressing: an **agent instance**.

The bus is hosted by the `tm` (trusty-mpm) daemon. This makes `tm` a hard,
already-accepted runtime dependency of `trusty-agents` (§4). Delivery follows
a strict two-level tree rooted at a user action (§5), per the ratified
orchestration model in
[ADR-0024](../adr/0024-subagents-in-process-only-assistants-communicate-not-delegate.md):
assistants are Level-0, always user-instantiated, and may communicate
laterally with other assistants; sub-agents are Level-1, in-process leaves
that never delegate and never talk out-of-process. Three distinct identifiers
— agent definition, agent instance, and caller — are established for the
first time (§6), because none of the five existing buses carries more than an
ad hoc subset of them, and the same three identifiers govern all three edges
uniformly. Channels (Slack, Telegram, …) are a **transport that originates a
user-identity message**, not a fourth participant kind, and they ride this
same bus rather than a parallel path (§8). The bus **is** the searchable log;
memory consolidation is the only sanctioned bridge from bus traffic into
`trusty-memory`'s curated index, and a promoted memory carries a provenance
pointer back to the message that produced it (§10).

**Why the peer-to-peer edge (§5.3) is load-bearing, not aspirational.**
PR #4240 (merged 2026-07-28, squash `5d99e385`) closed the one working
assistant-to-assistant interaction the codebase had — the tested Izzie ↔
cto-assistant peer-consult lane — as the direct, owner-authorized consequence
of [ADR-0024](../adr/0024-subagents-in-process-only-assistants-communicate-not-delegate.md)'s
kind-based delegation gate (assistants may never delegate to other
assistants, only to sub-agents). PR #4240's own description states this
plainly: *"This closes the Izzie ↔ cto-assistant peer-consult lane, and the
replacement mechanism does not exist... ADR-0019 is accepted but
unimplemented, and a bus-based messaging spec is being drafted separately."*
That spec is this one. §5.3 is therefore not a speculative future edge
alongside two already-real ones — it is the specified replacement for a
capability that is already live-removed from the codebase, and until it is
implemented, trusty-agents personas have **no** agent-to-agent messaging path
of any kind.

**Owner decision: §5.3 ships independently of ADR-0019.** The tm-session
lane — tmux-pane CLI sessions addressed via `session_send`,
`session_proxy_message`, and `tm sessions send` — is a separate,
still-unimplemented spec (ADR-0019, Accepted, with, per its own Consequences
section, no code landed since 2026-07-18 — see §2, §5.3). The persona peer
channel specified in §5.3 does not wait for ADR-0019, does not depend on it
landing first, and is not blocked by its absence; the two are independently
shippable. **Honest consequence, stated so a reader does not overclaim:**
until ADR-0019 lands, the "one bus" framing above is true at the **persona
layer only** — user↔assistant, assistant↔sub-agent, assistant↔assistant
(§5) plus channel traffic (§8). It is not yet true of the product as a
whole: the tm-session (tmux CLI) lane keeps whatever delivery mechanism it
has today, unaffected by and unintegrated with this spec, until ADR-0019 is
separately implemented.

**Owner decision: chat consolidates onto this bus.** User prompts, assistant
final responses, and peer messages (§5.3) all ride this bus as turn-level
messages — this bus is the system of record for chat, not only for the peer
channel it was originally scoped to close (see "Why the peer-to-peer edge...
is load-bearing" above). §5.1 (user ↔ assistant) is accordingly in scope for
the bus path on the same footing as §5.3, not a second-class or aspirational
edge. Two things explicitly do **not** ride this bus, for reasons stated
where each is specified: tool calls, which stay intra-turn execution-loop
metadata attached to the turn's response envelope rather than independent
bus traffic (§5.2), and streaming token deltas, which remain on the retained
per-crate buses for live rendering only (§11) — both excluded by the same
cost argument: modelling a high-frequency, request/response-shaped or
sub-message-granularity mechanism as durable, addressed bus envelopes would
build an RPC layer, or a UI telemetry feed, inside a fabric designed for
fire-and-forget delivery with a durable audit record. §9 records the
corresponding read/write split: the bus is the write path and durable truth
for chat; `trusty-memory`'s `chat_session` becomes a derived, read-side
projection rather than a second independent writer.

This is a DRAFT. It resolves what is answerable from the current codebase and
flags the rest explicitly in §12 for the owner.

## 2. Current state: five buses, zero of them addressed at agent granularity

Five independent, mutually-unaware broadcast implementations exist. None
imports or consumes another.

1. **trusty-mpm global hook/event bus.** `DaemonState.event_tx:
   broadcast::Sender<serde_json::Value>`
   (`crates/trusty-mpm/src/daemon/state/core.rs:192`). Untyped JSON,
   process-global, no addressing.
2. **trusty-mpm per-session `SessionEvent` bus.**
   `crates/trusty-mpm/src/control/event.rs` (`SessionEvent` enum, `actor.rs`
   holds the `broadcast::Sender<SessionEvent>`). Session-scoped by
   construction, but see the substring-match caveat below.
3. **`trusty-agents-common::events::bus`.** `HarnessEvent` /
   `HarnessPayload` over a process-global `OnceLock`-backed
   `broadcast::Sender` (`crates/trusty-agents-common/src/events/bus.rs`).
   Its own module doc calls out the ADR-0005 lineage; a repo-wide search for
   importers of `events::bus` outside its own crate returns **zero** —
   ORPHANED.
4. **trusty-agents `events.rs` bus.** A single `Event` enum with 20+
   variants (`SessionStarted`, `AgentMessage`, `ToolCalled`,
   `SlackMessageReceived`, `ListenerEventReceived`, …,
   `crates/trusty-agents/src/events.rs`) — the richest domain vocabulary of
   the five, but still a flat, unaddressed broadcast.
5. **trusty-code `events.rs` bus.** `SessionEventEnvelope` over a
   `OnceLock`-backed `broadcast::Sender`
   (`crates/trusty-code/src/events.rs`), session-scoped like #2 but a
   separate implementation in a separate crate.

**One genuinely ADDRESSED bus exists, at the wrong granularity.**
`crates/trusty-agents/src/bus/mod.rs` is a Unix-domain-socket `MessageBus`
whose `BusEnvelope` carries `source_project` and `target_project` (`None` =
broadcast). It is wired and live: `MessageBus::start` binds
`~/.trusty-agents/sockets/<id>.sock` and is called from `ctrl/repl/mod.rs`
and `runtime/startup.rs`. It addresses **projects**, not agents or personas —
useful prior art for the transport shape (NDJSON-over-socket, envelope +
broadcast fan-out) but not a foundation to extend, since project identity and
agent-instance identity are different axes (§6).

**Apparent per-session routing is not routing.** `GET /sessions/{id}/events`
(`crates/trusty-mpm/src/daemon/api.rs:166`,
`stream_session_events`) subscribes to the *same* global `event_tx` and
filters by testing whether the session UUID string appears inside the
serialized JSON payload (`crates/trusty-mpm/src/core/provisioning_stage.rs:129`
documents this explicitly as the existing substring-match mechanism, and
`api_tests.rs::session_events_sse_filters_by_session` pins the behavior).
Producers do not address a recipient; consumers filter client-side after the
fact. This is adequate for a debug SSE stream and inadequate as the addressing
primitive for a messaging bus.

**Durable audit logging already exists, but is scoped to the wrong thing.**
`crates/trusty-mpm/src/daemon/audit.rs` (`AuditLogger`) appends `AuditEntry`
records as JSONL to `<logs_dir>/overseer/YYYY-MM-DD.jsonl`, held on
`DaemonState.audit`. It is a working, durable, greppable append-only log —
exactly the shape §9 needs — but it only ever receives overseer allow/block/
respond/flag decisions, never bus traffic. Pointing it at bus envelopes is a
redirect of existing machinery, not new machinery.

**Personas are not tmux sessions.** trusty-agents personas run in-process
(ctrl-hosted async tasks / the `delegate_to_agent` and `dispatch_task`
mechanisms described in ADR-0024 §2), not as `tm`-managed tmux panes. `tm`'s
existing tmux-pane-based delivery (`tm sessions send`, per ADR-0019 built on
two literal `tmux send-keys` subprocess calls) has no pane to send keys to for
a trusty-agents persona and **cannot** be the transport for this spec's
delivery model. ADR-0019 already reached the adjacent conclusion for
`tm`-native IPC (superseding ADR-0005's deferred phases with a unified,
acknowledged, durable bus for `tm sessions send` and
`memory_send_message`) — this spec is the trusty-agents-side counterpart,
sharing the same host (§4) rather than re-deriving a second transport.

## 3. Bus retirement table

A spec that adds a sixth bus without retiring any of the five is a failure.
Verdicts:

| # | Bus | Verdict | Rationale |
|---|-----|---------|-----------|
| 1 | trusty-mpm global hook/event bus (`event_tx`) | **SURVIVES**, scope-narrowed | Stays as `tm`'s process-local debug/SSE fan-out for UI streaming (`GET /events`). It is not addressed and is not asked to become addressed — the new bus (§5) carries agent messaging; this bus keeps carrying UI telemetry. Its consumers (coordinator TUI, `GET /events`) are unaffected. |
| 2 | trusty-mpm per-session `SessionEvent` bus | **SURVIVES**, scope-narrowed | Same rationale as #1: session-lifecycle telemetry (backend spawn, stop reasons, stream-json passthrough) stays here. Cross-agent *messaging* moves off the substring-match `/sessions/{id}/events` path onto the new bus's addressed delivery (§5); the SSE endpoint keeps working for what it is actually good at — a debug tail. |
| 3 | `trusty-agents-common::events::bus` (`HarnessEvent`) | **DELETED** | Zero consumers by its own module doc and confirmed by a workspace-wide import search. Nothing to migrate. Delete the module; do not carry its `HarnessPayload` vocabulary forward — the new bus's envelope (§11) is defined fresh from actual delivery requirements, not backfilled from an unused design. |
| 4 | trusty-agents `events.rs` bus (20+-variant `Event`) | **SUBSUMED** | This is the richest existing domain vocabulary (`AgentMessage`, `ToolCalled`, `SlackMessageReceived`, `ListenerEventReceived`, …) and the closest thing to a real requirements list for the new bus's payload union. It does not survive as a separate transport: its UI-facing consumers (`repl/event_display.rs`, the SSE relay in `api/server/relay.rs`) are repointed to subscribe to the new bus, and its variant set seeds — but does not freeze — the new envelope's payload enum (`ToolCalled` explicitly does NOT carry forward: tool calls are excluded from bus traffic entirely, owner decision recorded in §5.2/§11). |
| 5 | trusty-code `events.rs` bus (`SessionEventEnvelope`) | **SURVIVES**, scope-narrowed | Same shape as #1/#2: it is trusty-code's session-attach replay/live continuity mechanism (`seq` is deliberately per-session, per its own module doc), not a cross-agent messaging path. It stays for that job. Any trusty-code-originated agent message (a workstream update that should reach an assistant) is published to the new bus, not re-derived from this one. |
| — | `crates/trusty-agents/src/bus/mod.rs` (project-scoped `MessageBus`) | **SUBSUMED** | The one bus that was already addressed, just at the wrong granularity (project, not agent instance). Its transport shape (Unix socket, NDJSON envelope, broadcast fan-out per connection) is the strongest prior art in the codebase for the new bus's local delivery mechanics, but project-level routing does not compose with the per-instance addressing §6 requires. `ctrl/repl/mod.rs` and `runtime/startup.rs` are repointed to the new bus; `source_project`/`target_project` become one addressing dimension inside the new envelope (§11), not a separate wire format. |
| — | `crates/trusty-mpm/src/daemon/audit.rs` (`AuditLogger`) | **SURVIVES**, target widened | Not a messaging bus — the durable JSONL sink §9 needs already exists. Widened to also receive bus envelopes (a second `logs_dir/bus/YYYY-MM-DD.jsonl` stream alongside the existing `overseer/` one), not rewritten. |

Net: **two buses deleted or subsumed outright** (#3, #4, plus the
project-scoped `MessageBus`), **three narrowed to the telemetry job they
already do well** (#1, #2, #5), and **one piece of existing durability
machinery reused** (audit logger). One new bus is added, hosted by `tm`.

## 4. Hard dependency on tm: absence/version-skew behavior

The owner has accepted that hosting the bus in `tm` makes `tm` a hard
dependency of `trusty-agents`. Three failure modes need one coherent answer
each.

**Daemon absent or stopped: FAIL-CLOSED for cross-instance delivery, degrade
(not refuse) for local operation.** An assistant or sub-agent MUST still be
able to start, run, and answer its invoking caller with no bus connection —
delegation within a single L0→L1 edge (§5) is in-process Rust, not a bus
round-trip, and does not need `tm` alive to function. What fails closed is
specifically: (a) publishing a message to any recipient other than the
in-process caller, (b) any lateral assistant↔assistant message, (c) any
inbound channel message, all of which return an explicit
`BusUnavailable`-shaped error to the caller rather than silently dropping.
This mirrors the existing precedent at
`crates/trusty-agents/src/bus/mod.rs`, where socket-connect failure is a
surfaced `Result`, not a swallowed no-op. Justification: a silent drop
recreates exactly the failure mode ADR-0019 was written to eliminate
("no way to distinguish 'message never sent' from 'sent but recipient never
polled'"); refusing outright to start would make every trusty-agents
persona unusable whenever `tm` is mid-restart, which is disproportionate to
what is actually broken (routing, not the persona's own reasoning loop).

**Version skew: the daemon publishes a bus protocol version at connect time;
a client on an incompatible major refuses to publish (fail-closed) but may
still consume in read-only/best-effort mode.** The existing `tm doctor`
diagnostic surface and `mcp__trusty-mpm__supervisor_status` tool are the
natural place to surface a skew warning to the operator; this spec does not
invent a second health-check path.

**Startup ordering:** `trusty-agents` processes MUST NOT block their own
startup on the bus becoming available — connect is attempted asynchronously
and retried with backoff, consistent with how `MessageBus::start` today binds
its socket without blocking the caller on a peer being reachable.

## 5. Delivery model: three edges over one tree

Per [ADR-0024](../adr/0024-subagents-in-process-only-assistants-communicate-not-delegate.md)
(ratified, same-day as this draft), the net shape is a strict two-level tree
rooted at a user action, with three distinct edges — each specified below to
the same depth: addressing, wake semantics, the three identifiers (§6), the
searchability path (§9), and the messages-vs-memories boundary (§10):

```
                 user
                  |  (5.1  bidirectional: user <-> assistant)
              assistant (L0) ---- 5.3 lateral ---- assistant (L0)
                  |  (5.2  bidirectional: assistant <-> sub-agent)
              sub-agent (L1, leaf)
```

### 5.1 User ↔ Assistant/PM

**Relationship.** Bidirectional. Every assistant is ALWAYS instantiated by a
user action (interactive session, a scheduled/cron trigger acting with
delegated user authority, or an inbound channel message treated as a user
action — §8). This edge rides the bus so the user-facing surface (CLI, TUI,
GUI, channel) can be swapped without the assistant knowing which one is
listening.

**Canonical for chat (owner-ratified 2026-07-28).** This edge is not merely
specified alongside §5.3 — it is where the bus becomes the system of record
for chat: a user's prompt and the assistant's completed final response are
both turn-level messages on this edge, addressed and durable exactly as
§5.3's peer messages are (§6, §9). This closes the gap between this
section's original design-time framing and the bus's actual role — the bus
is not a peer-only side-channel with user↔assistant as an
equally-specified-but-lower-stakes edge; it is the canonical chat transport,
full stop. What does NOT ride this edge as bus traffic: tool calls (§5.2) and
streaming token deltas (§11) — both excluded by a shared cost argument
stated where each is specified, not by this edge being partially in scope.

- **Addressing.** A user message addresses an assistant `definition_id`
  (§6a) — "talk to izzie" — resolved at send time to a live `instance_id`
  (§6b) when one exists for that user+definition pair, or queued when none
  does (§7). The user's own identity is the `from.kind = "user"` caller
  (§6c); no instance id is minted for the user side of this edge.
- **Wake semantics.** Full rule in §7. Summary: a running instance receives
  directly; a stopped/never-started definition queues to a durable inbox and
  is NOT auto-spawned by message arrival — only an explicit user
  instantiation (or an opt-in auto-resume policy) starts one.
- **Identifiers.** All three of §6 apply: `definition_id` always present on
  `to`; `instance_id` present on `to` once resolved; `caller` on `from` is
  `kind = "user"` (or `kind = "channel"` for a channel-originated user
  message, §8).
- **Searchability.** Every envelope on this edge is JSONL-logged per §9;
  `GET /bus/replay?instance=<id>` and `/bus/thread?message_id=<id>` cover it
  identically to the other two edges — no edge-specific query surface.
- **Messages vs. memories.** Governed by §10 uniformly: nothing on this edge
  is memory-eligible by default; promotion is consolidation's explicit act,
  carrying the `message_id` provenance pointer back to this edge's traffic
  exactly as it does for the other two.

### 5.2 Assistant/PM ↔ Sub-Agent

**Relationship.** Bidirectional. A sub-agent is an in-process LEAF: it never
delegates further (not up, not laterally, not to another sub-agent), never
talks out-of-process, and only ever responds to the one assistant that
invoked it. This edge is the in-process `delegate_to_agent` / `dispatch_task`
call today (ADR-0024 §2); this spec does **not** move it onto the network
bus — an in-process call has no addressing problem to solve, and forcing it
through the daemon would add a round-trip and a `tm`-availability dependency
to a path that today has neither. What DOES go on the bus is the **record**
of that exchange (§9) — durability and search, not delivery mechanics — so
the sub-agent's request/response is bus-searchable without the sub-agent
process itself being a bus publisher.

**Owner decision, generalized beyond this one edge: no tool call rides this
bus, ever** — not `delegate_to_agent`/`dispatch_task` above, and not any
other tool an assistant or sub-agent invokes (bash, git, file read/write,
search, …). Tool calls stay inside the agent's own execution loop and are
represented as structured metadata attached to the turn's response envelope
(§11), never as independent bus traffic. Rationale, stated explicitly rather
than left implicit: tool calls are intra-turn, high-frequency, and
request/response-shaped; modelling them as envelopes would force correlation
ids, ordering guarantees, partial-failure semantics, and timeouts into a
fabric designed for fire-and-forget delivery with a durable record — i.e.
building an RPC layer inside an audit bus — and would make every tool
invocation both a network hop and a durable write. This is the same cost
argument that already keeps token deltas off this bus (§11), applied to a
different high-frequency category. `delegate_to_agent`/`dispatch_task` above
are simply the one instance of this general rule that also happens to raise
its own addressing question (this edge) — the rule itself is broader than
this edge and governs every tool call an assistant or sub-agent makes.

- **Addressing.** Not bus-addressed at all — the invoking assistant's own
  process holds the direct in-process reference to the sub-agent task it
  spawned. There is no `definition_id`/`instance_id` resolution step because
  there is no delivery to solve (the call IS the delivery). The bus-side
  record of the exchange still carries `from`/`to` (§6) for search purposes,
  populated by the assistant after the in-process call returns.
- **Wake semantics.** Already fully answered by existing machinery (§7):
  `dispatch_task`/`delegate_to_agent` invocation IS the instantiation event.
  There is no "queued to an idle sub-agent" state — a sub-agent is never
  idle-and-addressable, so this spec adds nothing new here. This is the one
  deliberate asymmetry with §5.1/§5.3's queue-not-spawn rule, and it exists
  because a sub-agent has no independent, user-instantiation-gated existence
  to protect (§7).
- **Identifiers.** `definition_id` (§6a) names the sub-agent's config;
  `instance_id` (§6b) is minted at spawn by the invoking assistant, scoped to
  that one call's lifetime; `caller` (§6c) is always the invoking assistant's
  own `instance_id` — a sub-agent can never receive a message whose caller is
  anything else, by construction (ADR-0024's single-edge-leaf rule).
- **Searchability.** Identical mechanism to §5.1/§5.3 (§9) — the
  request/response pair is written to the bus JSONL log as the record of an
  in-process exchange, not as evidence of bus-mediated delivery.
- **Messages vs. memories.** Governed by §10 uniformly, with one added note:
  because this edge's "message" is a post-hoc record rather than a live
  delivery, the provenance pointer (§10) it contributes to a promoted memory
  points at the recorded exchange, not at a delivery event.

### 5.3 Assistant ↔ Assistant, Peer-to-Peer

**Relationship.** ADR-0024 decision 1 & 3: assistants "can communicate with
each other, but never delegate." **Decision: this lateral edge DOES ride the
same bus** as user↔assistant, not a private side-channel. Rationale: (a) it
needs the identical guarantees — durable, acknowledged, searchable, addressed
by agent instance — that motivated ADR-0019's rejection of `tm sessions send`
and `memory_send_message` for the equivalent `tm`-side problem; inventing a
second unaddressed channel for assistant-to-assistant traffic would reproduce
exactly the bug this spec exists to fix. (b) A lateral message and a user
message look identical from the receiving assistant's side — both are "a
message from a caller I must address a reply to" (§6) — so one delivery path
handles both without a special case. The only distinction the bus enforces is
that a lateral message's caller identity is another assistant's instance id,
never granting delegate authority (an assistant cannot make another assistant
delegate on its behalf — communication, not command).

**This edge closes a gap that is currently open, not a theoretical one — see
§1.** As of PR #4240 (squash `5d99e385`, merged today), no code path lets one
trusty-agents assistant persona reach another: the in-process
`delegate_to_agent` tool now structurally refuses any assistant→assistant
edge, at every tier pairing, per ADR-0024's kind predicate; ADR-0019's
unified IPC bus — the nearest candidate foundation — is Accepted but, per its
own Consequences section, unimplemented, with "no code... landed since
[2026-07-18]"; and ADR-0019's role model is itself built on ADR-0016's
singleton "ASSISTANT," a different sense of the word than `trusty-agents`'
plural per-persona population this spec addresses (§6a). Until this section
is implemented, **there is no substitute** for the closed Izzie ↔
cto-assistant lane.

- **Addressing.** Same shape as §5.1: a peer message addresses a
  `definition_id` (§6a) — one assistant naming another by its persona name —
  resolved to a live `instance_id` (§6b) when the target is running.
  **Instance bypass is in scope and supported:** a sender that already holds
  a live target `instance_id` (e.g. from a prior reply on the same thread)
  MAY address that instance directly, skipping definition-level resolution,
  for thread continuity. **Not decided here:** the failure-mode semantics —
  what happens when the targeted instance died between the sender learning
  that `instance_id` and the message actually being sent (fall back to
  definition-level resolution and queue, return an explicit delivery error,
  something else). That is being decided by the engineer implementing
  instance-bypass delivery; this spec deliberately does not pre-specify an
  answer that could contradict the landed implementation — see Open
  Question 8 (§12), narrowed accordingly.
- **Wake semantics.** Governed by the same rule as §5.1 (§7), explicitly: a
  peer message to a running instance delivers directly; a peer message to a
  stopped/never-started assistant definition queues to that definition's
  durable inbox and does **not** spawn a new instance — the queue-not-spawn
  rule is not a user-only protection, it applies identically when the
  sender is another assistant, because auto-spawning on ANY inbound message
  (peer or user) would manufacture an implicit user-instantiation event with
  no user behind it (§7). A peer message can never itself count as the "user
  action" that legitimately starts a new assistant instance.
- **Identifiers.** All three of §6 apply exactly as specified there: `to`
  carries the target's `definition_id` always and `instance_id` once
  resolved; `from.kind = "assistant_instance"` with the sending assistant's
  own `instance_id` and `definition_id` populated (§6c) — this is the field
  that structurally prevents a peer message from being mistaken for a user
  message or from carrying delegate authority (ADR-0024: communication, not
  command).
- **Searchability.** Identical mechanism to §5.1/§5.2 (§9) — no
  peer-specific query surface; `/bus/thread?message_id=<id>` walks a
  multi-hop peer conversation the same way it walks a user↔assistant thread.
- **Messages vs. memories.** Governed by §10 uniformly: a peer exchange is
  not memory-eligible by default and is promoted only through consolidation,
  carrying the same `message_id` provenance pointer. No special-casing for
  the peer edge is introduced.
- **Decline-ability.** Answered by derivation, not left open: per
  [ADR-0024](../adr/0024-subagents-in-process-only-assistants-communicate-not-delegate.md)'s
  virtual-twin authority principle — each assistant must take authority over
  its own actions, and that authority is not transferable — a peer message
  that carries a request for action MUST be decline-able. A non-decline-able
  request would compel the recipient to act on the sender's behalf, which is
  delegation under another name and indistinguishable in effect from the
  assistant→assistant edge ADR-0024 forbids. The envelope (§11) therefore
  needs an explicit accept/decline response shape for a peer *request*, as
  distinct from a plain informational message — the recipient's standing
  authority to decline is what keeps this edge coherent as communication
  rather than command. The exact wire shape of that accept/decline response
  is left to §11's eventual normative revision; this spec settles only that
  decline-ability itself is required, not optional.

**What is explicitly NOT a node in this tree:** a channel (Slack, Telegram)
is not a third participant kind; it is a transport that originates a
user-identity message onto the `user ↔ assistant` edge (§8). trusty-search,
trusty-memory, and the audit log are not tree nodes either — they are
consumers of the bus's durable log (§9, §10), never producers a message is
addressed to.

## 6. Identity model

Three identifiers, three lifecycles. None of the five existing buses carries
all three; establishing all three together is new work.

| Identifier | What it names | Lifecycle | Exists today? |
|---|---|---|---|
| **(a) Agent DEFINITION id** | The `agent.toml` / persona config (e.g. `izzie`, `cto-assistant`) | Static — versioned with the config file, changes only on edit | **Yes.** This is the agent name used throughout `crates/trusty-agents` config, `AgentConfig`, the deploy pipeline (DOC-42/agent-bundled-skills), and `tm agent show <agent>`. |
| **(b) Agent INSTANCE id** | One running invocation of a definition; the same definition may run many times concurrently (two users each running `izzie`, or one user running `izzie` twice) | Ephemeral — born at instantiation, dies at process/task exit | **Partial.** Session ids (`crates/trusty-mpm/src/control` `SessionEvent`, trusty-code's `session_id`) are the closest analogue but are scoped to `tm`'s own session model, not to a trusty-agents persona invocation as such. There is no single instance id that is stable across `tm`, trusty-agents ctrl, and trusty-code today. **This spec introduces one**: an instance id minted at the moment an assistant is instantiated (by a user action) or a sub-agent is spawned (by its invoking assistant), carried in every bus envelope as the addressable recipient/sender. |
| **(c) CALLER identity** | Who sent THIS message — a user, an assistant instance, or a channel-originated identity (§8) | Per-message — one caller identity per envelope, not a session-lived value | **No.** None of the five existing buses carries a caller field distinct from whatever ad hoc source tag its own envelope happens to define (`BusEnvelope.source_project`, `HarnessEvent`'s source metadata, etc. — all different shapes). This spec's envelope (§11) makes caller identity a normative, uniform field. |

**Why all three, not two.** Definition id alone cannot address "the specific
`izzie` I'm talking to right now" when two are running. Instance id alone
cannot answer "who may send this instance a message" or attribute a reply.
Caller identity alone, without instance id, cannot express "this reply goes
to the specific assistant instance that asked, not just any instance of that
definition." The three compose: a message is addressed `to: instance_id`
(resolved from a `definition_id` at send time only when no live instance is
targeted — see wake semantics, §7), and stamped `from: caller_identity`
which itself references an instance_id when the caller is an agent.

## 7. Wake semantics

A stopped or idle agent cannot receive anything by definition — the question
is what happens to a message addressed to one. One coherent answer, covering
all three edges of the tree (§5) — the rules below are edge-agnostic by
design: they key on the RECIPIENT's state (running instance vs. stopped
definition vs. sub-agent), never on whether the sender is a user or a peer
assistant, which is precisely what "apply coherently to all three" (§5.1,
§5.3) requires:

**Sub-agent invocation (assistant → sub-agent): delivery spawns an instance,
always.** A sub-agent has no durable existence between calls — `dispatch_task`
/ `delegate_to_agent` IS the instantiation event (ADR-0024 §2). There is no
"queued to an idle sub-agent" state to design for, because a sub-agent is
never idle-and-addressable; it exists only for the duration of one invoking
call. Wake semantics for this edge are therefore already fully answered by
existing machinery and this spec adds nothing here.

**Message to an assistant instance that is still running: direct delivery,
no wake needed.** The bus's local delivery mechanics (subscriber fan-out,
per the `MessageBus` prior art in §2/§3) push the envelope straight to the
subscribed instance.

**Message to an assistant DEFINITION with no running instance (stopped, or
never started): queued to a durable per-definition inbox — delivery does
NOT spawn an instance.** This is the deliberate asymmetry with the
sub-agent case, and it is deliberate for a specific reason: an assistant is
ALWAYS user-instantiated (§5) — spawning one is a decision with resource,
identity, and (per ADR-0024 decision 5) YOLO-risk-posture consequences that
must stay attached to an explicit user action, never to an implicit side
effect of someone else's message arriving. Auto-spawning an assistant on
message arrival would silently manufacture a user-instantiation event with
no user behind it. The queued message is delivered when: (a) the user (or a
lateral assistant's caller-visible request) next instantiates that
definition and the bus replays its durable inbox on connect, or (b) an
operator-configured auto-resume policy (existing precedent:
`mcp__trusty-mpm__auto_resume_set`) explicitly opts a definition into
spawn-on-message — an opt-in escape hatch, not the default.

**Inbound channel message (§8): does NOT unconditionally count as the user
instantiating an assistant.** It counts as a user-identity message arriving
on the `user ↔ assistant` edge (§5) addressed to a specific assistant
definition (channel routing determines which — e.g. a Telegram chat bound to
one assistant, a Slack channel bound to another, per existing per-agent
listener bindings, DOC-57 §6.1). Two sub-cases, both resolved by the same
rule as the paragraph above:

- **A running instance is already addressable for that user+definition
  pair** (the common case for an ongoing conversation): the channel message
  routes to it directly, no new instantiation.
- **No running instance exists:** the message is queued to the durable inbox
  (same mechanism as above), UNLESS that definition has an auto-resume /
  always-on policy configured (the existing pattern for a persistent
  channel-bound assistant, e.g. a Telegram-facing persona meant to always be
  reachable) — in which case arrival spawns the instance. This makes the
  channel case a specific, config-driven instance of the general assistant
  rule, not a separate design.

**Explicit non-goal:** this spec does not change sub-agent lifetime or add a
"durable sub-agent inbox" — ADR-0024 forecloses that by construction
(sub-agents have exactly one edge and no independent existence to queue
against).

## 8. Channels ride the bus

**Today:** an inbound Slack message reaches an agent through
`crates/trusty-agents/src/slack/handlers.rs`, which is one of the concrete
producers of the trusty-agents `events.rs` bus's `SlackMessageReceived`
variant (§2 item 4); a reply goes out via the paired `SlackReplySent`
variant and `api/server/relay.rs`. `trusty-channels` exists as a separate
crate but the live Slack path runs inside `trusty-agents` directly — the two
are not yet unified, which is itself part of the problem this spec is asked
to close (owner requirement 5: "not a parallel one").

**Decision: a channel is a transport that originates a user-identity
message, not a third participant kind.** It is not a tree node (§5). An
inbound Slack/Telegram message is translated, at the channel adapter
boundary, into exactly the same envelope shape (§11) a CLI/TUI/GUI-originated
user message would produce, addressed to whichever assistant definition that
channel/workspace/chat is bound to. This is what "same messaging path, not a
parallel one" means concretely: `trusty-channels` becomes the adapter that
produces bus envelopes, and the current `Slack*` variants in trusty-agents'
`events.rs` are subsumed into the bus payload union (§3, retirement #4)
rather than surviving as a Slack-specific side path.

**Caller identity carried by an inbound channel message (§6c):** three
values, all present, none collapsed into one — a channel message is
NOT anonymous the way a bare CLI keystroke is:

- `human_sender` — the platform-native sender identity (Slack user id,
  Telegram user id). This is the actual accountable human.
- `channel` — the platform conversation the message arrived on (Slack
  channel id, Telegram chat id) — needed for routing the reply back to the
  same place.
- `workspace` — the platform tenant/org (Slack workspace id, Telegram bot's
  own scope) — needed to disambiguate identically-named channels/users
  across tenants and to resolve which assistant binding applies.

All three are carried in the envelope's caller field as a structured
channel-origin identity, not flattened to a string — resolving "which local
user does this map to" (if any) is a separate, existing RBAC concern
(`ServiceTier`, `SLACK_RBAC_USERS`, DOC-57 §7.1 table row 3) that this spec
does not re-derive; the bus carries the channel-native identity faithfully
and lets the RBAC layer resolve it.

**Reconciling with the listener architecture (DOC-57 §6.1: listeners are API
connections, not MCP tools, with two-stage per-agent event filtering —
harness-level ingestion filter, then per-agent wake filter):** the two
mechanisms are **adjacent, not one subsuming the other.** A listener is how
an agent *reacts* to an external event stream it has opted into watching
(Gmail arriving, a calendar update) — the event is not, in general, addressed
to that agent by a caller; the agent's own `AgentListenerBinding.filter`
decides relevance. An inbound channel message in this spec's sense is
different in kind: it IS addressed, by a human sender, to a specific
assistant, exactly like a CLI message — there is no relevance filter to
apply because the human already chose the recipient by which channel/chat
they used. Concretely: Gmail/Calendar stay on the listener path (§ DOC-57
§6); a direct Slack DM or a bound Telegram chat's message is bus traffic
under this spec. The boundary is address-by-sender vs. filter-by-subscriber,
and it is possible for the *same* connector (Slack) to have both a listener
binding (watching a channel for mentions/keywords) and a bus-carried direct
message (a DM to the bot) simultaneously — they are not mutually exclusive
uses of one connector.

## 9. Searchability: storage, retention, indexing, query surface

**Storage: redirect the existing audit logger, don't build a new store.**
`crates/trusty-mpm/src/daemon/audit.rs`'s `AuditLogger` already writes
append-only, daily-rotated JSONL under `<logs_dir>/<subsystem>/YYYY-MM-DD.jsonl`
with a proven never-fail-the-hot-path write discipline (`AuditLogger::log`
never propagates IO errors). This spec adds a sibling stream,
`<logs_dir>/bus/YYYY-MM-DD.jsonl`, fed by every envelope that crosses the bus
(§11) — request, response, and delivery-state transitions alike. No new
storage engine, no new dependency: the durability requirement is met by
widening a component that already meets it for a narrower case.

**Retention:** daily-rotated JSONL files age out on the same policy surface
`tm`'s existing log-pruning already uses (DOC-33, tm meta-harness logging —
per-delegation observability with a documented pruning story); this spec
does not invent a second retention knob. Default retention window is an open
question (§12) — cost-sensitive, not a design question this spec can settle
from code alone.

**Indexing: the bus exposes its own query/replay API; trusty-search does
NOT index it as a first-class corpus.** Reasoning: trusty-search's cost model
(per prior work: eager warm-boot, BM25 second-copy, one-way HNSW promotion —
see the trusty-search memory-footprint findings already on file) is tuned for
relatively low-churn source-code and document corpora, indexed once and
queried many times. Bus traffic is the opposite shape — high-churn,
append-only, time-ordered, and the dominant query pattern is "everything
between t0 and t1 for instance X" (a replay), not semantic similarity search.
A dedicated replay API over the JSONL stream (linear scan bounded by date
partition + in-file binary search on the RFC3339 timestamp prefix, mirroring
`AuditLogger`'s existing per-day file layout) is cheap, requires no daemon
beyond `tm` itself, and matches the access pattern. **Concrete cost
comparison:** indexing 100% of bus traffic into trusty-search's HNSW+BM25
pipeline pays embedding cost on every message and holds a live in-memory
index over data that is read almost exclusively by timestamp range, not by
similarity — the wrong tool for the dominant query. The JSONL replay API pays
approximately zero marginal cost beyond the write `tm` already performs.

**Query surface:** a new `tm bus` (or daemon HTTP) surface —
`GET /bus/replay?instance=<id>&since=<ts>&until=<ts>` and
`GET /bus/thread?message_id=<id>` (walk `in_reply_to`, §11) — modeled
directly on the existing `GET /sessions/{id}/events/poll` pattern
(`crates/trusty-mpm/src/bin/tm/commands/session.rs:204`) rather than
inventing a new HTTP idiom. **Exception, not contradiction:** if a later
product need emerges for semantic search *over* bus history (e.g. "what did
we ever discuss about X"), that is a `trusty-search` *attached index* per
DOC-58's already-shipped K-d attached-index mechanism — pointed at the same
JSONL files as a read-only secondary corpus, opt-in, not the default query
path. This keeps the mandatory cost (JSONL write) flat and makes the
expensive path (semantic indexing) something an operator turns on, not
something every message pays for.

**Read model / write model split, owner-ratified 2026-07-28: `chat_session`
becomes a derived projection, not a second writer.**
`<logs_dir>/bus/YYYY-MM-DD.jsonl` (above) is an append-only audit sink, not a
query surface — rehydrating a chat view needs indexed query by agent, time
range, and workstream, which scanning daily JSONL cannot serve and which
degrades as the file set grows. The bus is therefore the WRITE path and
durable truth for chat; the existing `trusty-memory` `chat_session` keyed
`persona-{agent}` (`crates/trusty-agents/src/ctrl/pm_task/dispatch/
persona_memory.rs`, `session_id_for`, DOC-54 §9.1 — "each agent maintains ONE
continuous conversation per session") becomes a DERIVED PROJECTION fed from
the bus, and is the surface the UI actually reads for chat rehydration.
`persona_memory.rs`'s `spawn_persist_turn` stops being an independent writer
once the projection is fed from the bus. **Explicit prohibition:** chat MUST
NOT be dual-written as two independent durable records (the bus JSONL AND
`chat_session`) — two independently-written copies of the same truth drift,
and this section's own storage rationale above (one write path, `tm`'s
existing durability discipline widened, not duplicated) is undermined the
moment a second writer exists.

**Sequencing.** Today, `spawn_persist_turn` writes `chat_session` directly —
§5.1's bus-side user-edge publishing and the replay/thread query surface
above do not exist yet. Chat-view rehydration ships FIRST against
`chat_session` as it is written today, ahead of and independent from this
spec's bus-side delivery landing. When §5.1's user-edge publishing and the
replay/thread query surface land, the WRITER is re-pointed at the bus —
`chat_session` is populated by consuming bus traffic instead of by direct
persistence inside the persona dispatch path — and the frontend is
unchanged, because the UI reads the projection either way. This makes the
near-term `chat_session`-direct fix a step toward the target architecture,
not throwaway work to be later discarded.

## 10. Messages vs. memories

**Normative constraint, restated as a rule this spec is bound by:** the bus
is the log; `trusty-memory` is the curated index; consolidation is the ONLY
sanctioned bridge between them. Nothing in this spec's design writes every
bus message into memory. `trusty-memory`'s existing blocklist, dedup window,
and short-content gates exist precisely because unscoped auto-capture already
flooded the palace once — this spec's bus is a second, larger-volume source
than whatever produced that flood, so respecting those gates is load-bearing,
not optional politeness.

**What actually happens to bus traffic by default: nothing, beyond §9's
JSONL log.** Promotion to memory is an explicit act, performed by the
existing consolidation surface (`mcp__trusty-memory__dream_consolidate_room`
/ `palace_dream`), which already runs against a room's accumulated content on
its own schedule and through its own gates. This spec's only change to that
picture is making bus history one more thing consolidation *can* read from —
via the replay API (§9), not a new firehose subscription — when a room's
consolidation pass runs. Bus messages are not memory-eligible by default;
they become eligible the same way any other room content does, subject to
the same blocklist/dedup/short-content gates already enforced.

**Provenance pointer: YES, required.** A memory promoted from bus content
MUST retain a pointer back to the originating message(s) — concretely, the
memory record carries the bus `message_id` (§11) (or a small ordered set,
for a consolidation that merges several messages into one memory) it was
derived from. This closes the gap the seed findings call out explicitly:
neither system today can answer "why do I believe this?" A provenance
pointer is cheap to carry (one more indexed field) and is the only way a
future audit, correction, or "who said this and when" query resolves without
re-deriving it by hand. This spec does not design the memory-record schema
change itself (that is `trusty-memory`'s surface) — it specifies the
requirement and the field the bus side must expose (`message_id`,
globally unique, stable across the JSONL log's rotation) for
`trusty-memory` to reference.

**Distinct from §9's `chat_session` projection.** The `chat_session` derived
projection (§9) is a complete, uncurated per-turn rehydration surface — it
exists so a chat view can be redrawn, not so content becomes memory-eligible.
It is unrelated to the curated-memory promotion this section governs:
populating `chat_session` from the bus is not consolidation, carries no
promotion decision, and does not make bus content memory-eligible by itself.
The two pathways — chat rehydration (§9) and curated memory promotion (this
section) — read the same underlying bus log but serve different purposes,
and neither substitutes for the other.

## 11. Envelope schema (illustrative)

Not normative wire format — illustrates how §6's three identifiers, §5's
tree edges, and §8's channel-origin identity compose into one envelope
shape, seeded from the existing `BusEnvelope` (§2/§3) and the `events.rs`
payload vocabulary (§3 retirement #4) rather than invented from scratch.

```jsonc
{
  "message_id": "01J...",                 // ULID/UUID, globally unique, stable — the §10 provenance key
  "ts": "2026-07-28T13:00:00Z",
  "in_reply_to": "01J... | null",          // threads a reply chain for §9's /bus/thread

  "edge": "user_assistant | assistant_subagent | assistant_assistant",

  "from": {                                // caller identity, §6c
    "kind": "user | assistant_instance | channel",
    "instance_id": "izzie#a1b2c3 | null",  // §6b, present when kind = assistant_instance
    "definition_id": "izzie | null",       // §6a
    "channel_origin": {                    // §8, present only when kind = channel
      "connector": "slack | telegram",
      "human_sender": "U012ABC",
      "channel": "C0123",
      "workspace": "T0123"
    }
  },

  "to": {
    "instance_id": "cto-assistant#d4e5f6 | null",  // resolved recipient, when live
    "definition_id": "cto-assistant"               // always present — resolves via §7 wake rules when instance_id is null
  },

  "delivery_state": "queued | delivered | acked | dropped",  // ADR-0019's acknowledgment requirement, carried into this bus too

  "payload": {
    "type": "chat_text | ... ",              // deliberately NOT "tool_call" — see below
    "tool_calls": [ /* structured metadata only, §5.2 */ ],
    "...": "..."
  }  // seeded from events.rs's variant union, §3 #4
}
```

**Explicit boundary, owner-decided: streaming chat token deltas do NOT ride
this bus.** The `payload.type = "chat_text"` shown above is a complete
message (one finished turn), not a live token-by-token stream. Token-level
streaming deltas continue to ride the existing per-crate buses this spec
retains for that exact job — the `tm` global and per-session event buses
(§3 #1, #2) and trusty-code's `SessionEventEnvelope` bus (§3 #5) — precisely
as those retirement-table verdicts already state they survive "for UI
streaming." This section makes that split explicit rather than leaving it
implied by the retirement table's rationale column alone: token deltas are
high-volume UI telemetry (potentially dozens of partial updates per second
per active response), not durable messages, and routing every delta through
this bus's JSONL audit sink (§9) would flood it with content that has no
standalone meaning once the turn completes and no reason to be individually
searchable, threaded (`in_reply_to`), or retained. Only the completed turn —
the payload this envelope actually carries — is bus traffic; the deltas that
assembled it are not.

**Second explicit boundary, owner-decided: tool calls do NOT ride this bus
either.** The payload schema above deliberately does not include a
`tool_call` type, despite `events.rs`'s `ToolCalled` variant (§3 #4) being an
available candidate — it is not carried forward. §5.2 states the full
rationale (request/response-shaped, high-frequency, would force an RPC
layer's guarantees onto a fire-and-forget fabric); this section states its
consequence for the wire shape: a completed turn's envelope may carry a
`tool_calls` array as descriptive metadata — which tools ran, in what order,
summarized — but each individual tool call is never itself published,
addressed, or durably logged as its own envelope.

**Reconciling "every turn-level message rides the bus" (§1, §5.1) with "no
deltas ride the bus" (above): these are not in tension.** A turn produces
exactly ONE bus envelope — the completed response — regardless of how many
intermediate token deltas or tool calls occurred while producing it. Deltas
stream over the legacy per-crate buses (§3 #1/#2/#5) for live rendering only
and are never durable bus traffic; tool calls are metadata inside the one
completed envelope (above), never separate traffic. "Chat consolidates onto
the bus" means the finished turn is canonical here — not that every event
that occurred while producing it is.

## 12. Open questions

These cannot be resolved from the current codebase; the owner answers them,
not this spec.

1. **Retention window for `<logs_dir>/bus/*.jsonl`.** §9 reuses DOC-33's
   pruning surface but does not set a default number of days — bus volume at
   steady state (especially once channels, §8, ride it) is unknown without
   production data, and this trades directly against disk cost.
2. **Version-skew policy specifics (§4).** This spec establishes fail-closed
   on publish / best-effort on consume for an incompatible major version, but
   does not define the actual version negotiation handshake or what counts
   as "incompatible" (major-only, or a finer compatibility matrix).
3. **Auto-resume opt-in mechanics for channel-bound assistants (§7).** The
   spec reuses `mcp__trusty-mpm__auto_resume_set` as the existing precedent
   for spawn-on-message, but does not design the config surface that marks a
   specific assistant definition as "always-on for channel X." Is this
   per-definition, per-channel-binding, or both?
4. **Cross-project lateral messaging.** §5's assistant↔assistant edge is
   scoped to "communicate, never delegate," but does not resolve whether an
   assistant in project A can address an assistant instance in project B
   (the old `MessageBus`'s actual job, §2/§3) — or whether lateral
   communication is scoped to one project's roster only.
5. **Bus protocol version negotiation transport.** Where does a client learn
   the daemon's bus protocol version — a new field on an existing
   `supervisor_status`-shaped response, or a dedicated handshake message on
   first connect? Not decided here.
6. **Definition-id uniqueness across the whole system vs. per-project.**
   §6a treats definition id as effectively global (`izzie`), but agent
   configs are deployed per-project (DOC-42/agent-bundled-skills) — whether
   two projects' same-named definitions are the same identity for bus
   addressing purposes, or namespaced by project, is unresolved.
7. **What triggers consolidation to actually read bus history (§10).**
   Today's `dream_consolidate_room` runs against a room's existing content
   on its own schedule; whether/how it is pointed at the bus replay API (poll
   on a cadence, triggered by a threshold, or manual) is not designed here.
8. **Failure-mode semantics for instance-bypass delivery (§5.3).** §5.3 now
   settles that instance bypass is in scope and supported: a sender holding
   a live peer `instance_id` MAY address it directly, bypassing
   definition-level resolution. What remains open is what happens when the
   targeted instance died between the sender learning that id and the
   message being sent — fall back to definition-level resolution and queue,
   return an explicit delivery error, or something else. This is being
   decided by the engineer implementing instance-bypass delivery, not
   settled here, so this spec does not risk contradicting the landed
   implementation.

   *(Former Open Question 8 — whether a peer message may address a
   definition or a specific instance at all — is no longer open: instance
   bypass is now normative text in §5.3.)*
9. **Is there any ordering or delivery guarantee between peers (§5.3)?**
   §11's `delivery_state` field tracks a single envelope's own lifecycle
   (queued/delivered/acked/dropped), but does not establish whether two
   peer messages from the same sending instance to the same target are
   guaranteed to arrive in send order, or whether a peer conversation may
   interleave with messages from other senders with no ordering guarantee
   at all. This spec does not resolve it.

   *(Former Open Question 9 — whether a peer request may be decline-able —
   is no longer open: it is answered by derivation from ADR-0024's
   virtual-twin authority principle and is now normative text in §5.3.)*

10. **How is a §5.1 user-edge publish authenticated?** PR #4261's
    caller-identity fix (now on branch `feat/peer-bus-daemon-foundation`)
    requires every publisher on the peer path to be a REGISTERED INSTANCE:
    publish admits only `kind: assistant_instance`, resolves the claimed
    `instance_id` through the registry, and re-stamps `definition_id` from
    the registration so a sender cannot choose its own attribution.
    `CallerKind::peer_edge()` is a total match and `user`/`channel` are
    REJECTED, not mapped — deliberately, because routing them there is the
    delegation hole ADR-0024 closes. A user prompt has no instance
    registration, so admitting the §5.1 edge onto the same publish path is
    NOT a one-line match-arm addition: it needs a stated authentication rule
    for user-originated publishes that does not reopen the forgery path
    (an assistant publishing as `kind: user`, and a receiving assistant
    treating that as a genuine user instruction — reconstituting the
    assistant→assistant delegation ADR-0024 closes, under a different
    label). This spec frames the question; the answer is left to the owner
    and the engineer implementing §5.1's bus-side publishing.

## 13. DOC-N numbering note

Per DOC-38 §4.1's scan-before-claim rule, `docs/specs/README.md`'s catalog was
scanned before assigning this spec's number, rather than trusting its own
"next free" hint blindly. That scan found the hint (`DOC-60`, noted
2026-07-27) still correct, and surfaced **two already-double-booked numbers**
that a naive scan could re-collide with:

- **`DOC-42` is double-booked.** `docs/specs/agent-bundled-skills.md` (merged,
  main) self-labels `DOC-42` — Agent-Bundled Skills. Separately, PR #3006
  ("DOC-42: Engineering Lead / Virtual Twin Cross-Tool Orchestration
  Architecture", branch `spec-twin-lead-architecture`, now CLOSED) also
  claimed `DOC-42`, and `docs/adr/0016-orchestration-hierarchy-lead-pm-assistant.md`
  still textually references "DOC-42 (Engineering Lead / Virtual Twin
  architecture, PR #3006...)" as an open reconciliation item. The
  `spec-twin-lead-architecture` branch's live claim has since moved to
  `DOC-44`/`DOC-45` (per the README's own catalog note), but the stale
  `DOC-42` reference in ADR-0016 is not corrected in this PR — out of scope,
  flagged here per this task's instruction not to claim `DOC-42`.
- **`DOC-46` is double-booked.** `docs/specs/DOC-46-adr-standard.md` (merged
  via PR #3172) is the catalog's canonical `DOC-46` (ADR standard). But
  **issue #3169** ("[BUS-13] File DOC-46: Control Bus Architecture spec doc",
  still OPEN) reserved `DOC-46` for exactly the topic of this spec — a
  control-bus architecture doc, written last "once the shape has proven out
  in code (BUS-1 through BUS-6 stabilized)." PR #3172 landed first and took
  the number issue #3169 had reserved. Issue #3169 is the direct predecessor
  of this spec's subject matter but its claimed number is no longer free;
  this spec supersedes issue #3169's intent under a new number rather than
  reopening the `DOC-46` collision a second time. A follow-up should close
  or retarget #3169 to point at `DOC-60`.

**Number claimed by this spec: `DOC-60`** — the next free slot after the
highest cataloged entry (`DOC-59`), confirmed free by: no file in
`docs/specs/` self-labels it, no merged or open PR title references it
(`gh pr list --search "DOC-60 in:title"` — empty), and no open issue title
references it. A catalog row should be added to `docs/specs/README.md` when
this spec leaves DRAFT status (not done in this PR, matching this task's
draft-only, no-commit scope).
