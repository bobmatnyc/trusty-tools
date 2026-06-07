# 0005. Shared HarnessEvent envelope + process-global event bus in trusty-agents-common

- **Status:** Accepted
- **Date:** 2026-06-06
- **Scope:** Workspace-wide (`trusty-agents-common` foundation; future consumers `trusty-agents`, `trusty-mpm`, `trusty-code`)
- **Supersedes / Superseded by:** Builds on ADR-0004 (three harnesses on a shared event-driven common)

## Context

ADR-0004 established that the three harnesses — `trusty-code` (coding),
`trusty-mpm` (meta/control-plane), and `trusty-agents` (agentic) — should share
an event-driven foundation in the common layer. Today each harness streams
real-time telemetry its own way:

- `trusty-agents` has the richest taxonomy: `crates/trusty-agents/src/events.rs`
  defines a ~21-variant `Event` enum (`#[serde(tag = "type")]`) covering the
  full PM lifecycle (session/agent/tool/phase/LLM/persona/recap), plus a `Ping`
  keepalive, a process-global `OnceLock<broadcast::Sender<Event>>` bus
  (capacity 1024), `publish`/`emit` helpers, and a `__OMPM_EVENT__ ` stderr
  relay prefix for child-to-parent re-broadcast.
- `trusty-mpm` relays Claude Code **hooks** — open-ended `{kind, data}` events
  whose taxonomy is not fixed and should not be modelled variant-by-variant.
- `trusty-code` will grow its own coding-session events.

A single subscriber (UI, SSE relay, aggregator) cannot today consume all three.
We need one envelope and one bus that all three harnesses emit onto, while
keeping each harness free to evolve its own payload taxonomy.

This ADR records the Wave 3 **Phase 0** decision: land the foundation type, the
bus, the subscription API, and a filter in `trusty-agents-common` — **additively,
with no consumers wired yet** (tracked in issue #875, epic #830, refs #833).
The existing `trusty-agents::events::Event` enum is left untouched; its variants
are *copied* into a new `LifecycleEvent` so Phase 0 is a pure addition with zero
behavior change. De-duplication and consumer migration are deferred to P1–P4.

## Decision

We will add an `events` module to `trusty-agents-common`, split into focused
submodules to respect the 500-line cap (`mod.rs` facade + `lifecycle.rs` +
`bus.rs` + `filter.rs`), exposing the following surface.

### Envelope shape

- **`HarnessSource`** — `{ Agents, Mpm, Code }`, snake_case serde. Identifies
  which harness produced an event so routing is O(1) without payload inspection.
- **`LifecycleEvent`** — a verbatim copy of `trusty-agents::events::Event`'s
  variants **except `Ping`**, keeping `#[serde(tag = "type", rename_all =
  "snake_case")]` and every per-variant field, plus a `session_id()` accessor.
- **`HarnessPayload`** — `#[serde(tag = "domain", content = "event")]` with arms
  `Lifecycle(LifecycleEvent)`, `Hook { kind: String, data: serde_json::Value }`,
  and `Ping`. Wire shape: `{"domain":"lifecycle","event":{...}}`,
  `{"domain":"hook","event":{"kind":...,"data":...}}`, `{"domain":"ping"}`.
- **`HarnessEvent`** — the envelope: `{ source, session: Option<String>
  (skipped when None), seq: u64, at: DateTime<Utc>, payload: HarnessPayload }`.

### Bus + subscription API

- A process-global `static EVENT_BUS: OnceLock<broadcast::Sender<HarnessEvent>>`
  (capacity 1024), with `bus()` and `subscribe()`.
- A monotonic `static SEQ: AtomicU64` so `publish`/`emit` stamp `seq` and `at`
  (`Utc::now()`) automatically. `publish` is best-effort (ignores `SendError`
  when there are no subscribers) and returns the assigned `seq`.
- `emit` = `publish` + one NDJSON stderr line prefixed with the public
  `EVENT_LINE_PREFIX = "__HARNESS_EVENT__ "`. The line formatting is factored
  into `format_event_line` so it is unit-testable without capturing real stderr.
- A lagged-receiver helper `recv_with_lag` that translates
  `broadcast::error::RecvError::Lagged(n)` into a public typed `Lag { skipped }`
  notice (returned as the `Err` arm of an inner `Result`) and resumes the stream
  rather than dropping it; `Closed` maps to the outer `Err(())`.

### Filter

- `Filter { source: Option<HarnessSource>, session: Option<String>, domains:
  Option<Vec<&'static str>> }` with `matches(&HarnessEvent) -> bool`. Present
  constraints are ANDed; `Filter::default()` matches everything. Domain strings
  are `"lifecycle" | "hook" | "ping"`. We deliberately ship the lightweight
  `matches` predicate (no new Stream dependency); subscribers will call it in
  their `recv_with_lag` loop in later phases.

### Secondary design defaults (recorded for posterity)

1. **MessageBus stays separate.** This event bus carries *telemetry* (fan-out,
   lossy-under-lag, no delivery guarantee). It is intentionally NOT a
   command/RPC bus; any future request/response `MessageBus` is a distinct
   abstraction and is out of scope here.
2. **In-process + stderr relay only.** Phase 0 scope is the in-process
   broadcast channel plus the stderr child-to-parent relay prefix. No network
   transport (SSE/WebSocket wire format) is defined here — see the risk below.
3. **`session: Option<String>` key.** Correlation is a single optional string,
   not a typed newtype, matching the existing `session_id: String` fields and
   keeping the envelope cheap. A session-less event (e.g. a bare `Ping`) never
   matches a `Filter` session constraint.

### Adapt, don't fold (hooks)

Rather than folding hook events into the lifecycle enum (which would force every
open-ended Claude Code hook to become a typed variant), `HarnessPayload` tags by
**domain** and gives hooks an untyped `{kind, data: Value}` arm. Each harness
adapts its native events into a domain arm; we do not fold disparate taxonomies
into one mega-enum.

## Consequences

**Easier:**

- One subscriber can consume lifecycle, hook, and keepalive events from all
  three harnesses over one bus, ordered by `seq` and attributed by `source`.
- Adding a harness or a new lifecycle variant does not touch the envelope.
- Hook events flow through without per-event modelling work.
- The foundation is fully unit-tested (serde round-trips per arm, seq
  monotonicity, the filter matrix, lagged-then-resume against a constructible
  test bus, and emit-line formatting) before any consumer depends on it.

**Harder / trade-offs:**

- `LifecycleEvent` is, for now, a **copy** of `trusty-agents::events::Event`.
  Until P1/P2 migrate `trusty-agents` onto it, the two definitions can drift;
  the duplication is an accepted, time-boxed cost of a zero-risk additive
  Phase 0.
- `trusty-agents-common` gains a regular (non-dev) `tokio` dependency. The
  workspace `tokio` already enables `"full"` (which includes `sync`/`broadcast`),
  so no feature override is needed, but the crate is no longer tokio-free.

**Neutral / follow-up — phased migration plan:**

- **P0 (this ADR, #875):** land types + bus + filter + tests, no consumers.
- **P1:** migrate `trusty-agents` emit sites onto `HarnessEvent`
  (`source = Agents`), collapse the copied `LifecycleEvent` back to a single
  definition, and re-export for source compatibility.
- **P2:** route `trusty-mpm` hook relays through `HarnessPayload::Hook`
  (`source = Mpm`).
- **P3:** emit `trusty-code` coding-session events (`source = Code`).
- **P4:** define the SSE/WebSocket **wire shape** for browser/Tauri
  subscribers and the network relay.

**Key risk — SSE wire shape.** Phase 0 fixes the *internal* JSON shape of
`HarnessEvent` but does NOT commit to the *external* SSE/WebSocket contract
(P4). If the internal shape leaks directly onto the wire before P4 deliberately
designs that contract, we risk freezing an accidental public API. Consumers in
P1–P3 must treat `HarnessEvent`'s JSON as an internal detail until P4 ratifies
the external schema.
