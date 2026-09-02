---
spec_refs:
  - id: SPEC-TCUI-17~draft
    path: docs/specs/trusty-code-harness-ui.md
    anchor: SPEC-TCUI-17~draft
---

# DOC-73 — The Unified mpm/code/agents Dashboard: One Event Stream, List and Tree

**Status:** Draft. Design record only — no code in this PR.
**Spec ID:** `SPEC-UNIDASH-01~draft` … `SPEC-UNIDASH-11~draft`
**Subsystem:** `trusty-console` — the dashboard routes, the bus reader, and the
SSE fan-out (`src/routes/`, `src/metrics_poller.rs`, `ui/src/`);
`trusty-agents-common` — the `HarnessEvent` envelope the taxonomy extends
(`src/events/`); `trusty-common` — the prospective `control_bus` home (#3157);
`trusty-mpm`, `trusty-code`, `trusty-agents` — the three event sources and their
adapters.
**Owner:** Bob Matsuoka
**Last-updated:** 2026-09-02
**DOC-N claim:** `DOC-73`, scan-before-claim per DOC-38 §4.1.
`docs/specs/README.md`'s catalog note names `DOC-72` as the next free number;
`DOC-72` is claimed by open pull request
[#6607](https://github.com/bobmatnyc/trusty-tools/pull/6607) on branch
`docs/6606-analyze-lsp-spec`, so this spec takes the next number after it. A
repo-wide grep for `DOC-73` over `origin/main` and over that branch returned only
the catalog note; `scripts/check_doc_numbers.sh` reported 132 docs / 126 claims,
3 grandfathered, 0 violations before this file was added.
**Builds on:** the owner directive of 2026-09-02 (§0);
[ADR-0005](../adr/0005-harness-event-bus.md) (the `HarnessEvent` envelope and the
process-global bus in `trusty-agents-common`);
[ADR-0019](../adr/0019-unified-ipc-messaging-on-event-bus.md) (the control bus
carries addressed messages as well as telemetry);
[ADR-0032](../adr/0032-no-service-owns-http-console-is-the-only-http-surface.md)
and [ADR-0035](../adr/0035-console-health-probe-aggregates-over-uds.md) (no
sibling daemon binds HTTP; console is the only HTTP surface and aggregates over
UDS); [DOC-39](./trusty-code-harness-ui.md) §8 (the Foundry design system).
**Cross-ref:** issues
[#6611](https://github.com/bobmatnyc/trusty-tools/issues/6611) (this spec),
[#3157](https://github.com/bobmatnyc/trusty-tools/issues/3157) (the control-bus
epic whose consumer side this dashboard settles),
[#6155](https://github.com/bobmatnyc/trusty-tools/issues/6155) (embedded tool UIs
migrate into console — the precedent that puts this dashboard in console rather
than in each crate),
[#6516](https://github.com/bobmatnyc/trusty-tools/issues/6516) (the
machine-status dashboard epic these views sit beside),
[#6519](https://github.com/bobmatnyc/trusty-tools/issues/6519) (the
`/ui/screensaver` route this spec's screensaver extends),
[#6606](https://github.com/bobmatnyc/trusty-tools/issues/6606) and
[DOC-72](https://github.com/bobmatnyc/trusty-tools/pull/6607) §4 (the analyze-side
console relay — the first feeder into the bus this spec generalizes).

---

## 0. Owner directive

> "I want to also build a user story/spec for the mpm/code/agents dashboard. It
> should be a single codebase that supports all three. The basic concept is that
> it ad an event stream that supports filterable list or caller tree views. Each
> agent action triggers a typed event. Types are workflow (start, stop, spawn,
> read, write), agent, file etc. the list view is sequential, the tree view is a
> call graph. A single event bus service handles all events, and the dashboard
> builds by showing events in list or tree format. Lists scroll, tree spawns
> nodes while the node is active. The dashboard has a viewer for all object types
> linked from the event node. Use a fast stylish viz library to render. Each
> (list/tree) should be its own non interactive. screensaver too."
>
> — Bob, 2026-09-02

Six requirements are read out of that directive and bind every section below.

1. One codebase serves all three harnesses. Not three dashboards.
2. Every agent action emits a typed event. The type carries the meaning, so the
   renderer never parses prose.
3. One event bus service handles every event. Not one bus per harness.
4. Two views read the same stream: a sequential list and a call-graph tree.
5. Each view is a standalone non-interactive route, and each also runs as a
   screensaver.
6. Every event node links to a read-only viewer for the object it names.

---

## Why

The three harnesses each grew their own event enum, their own bus, and their own
consumer, and nothing can watch them together.

- `trusty-agents` has a 26-variant `Event` enum and its own process-global
  `OnceLock<broadcast::Sender<Event>>` (`crates/trusty-agents/src/events.rs`).
- `trusty-code` has its own `Event` enum plus a `SessionEventEnvelope` whose
  `seq` is per-session, assigned by `session::registry::SessionRegistry`
  (`crates/trusty-code/src/events.rs:192`).
- `trusty-mpm` has a ten-variant `SessionEvent` keyed on `ControlSessionId`
  (`crates/trusty-mpm/src/control/event.rs:119`), and separately relays Claude
  Code hooks through a `HookEvent` enum carrying the upstream PascalCase wire
  names (`crates/trusty-mpm/src/core/hook.rs:25`).

ADR-0005 already decided the fix: one `HarnessEvent` envelope,
`{source, session, seq, at, payload}`, in `trusty-agents-common`
(`crates/trusty-agents-common/src/events/bus.rs:121`). That decision landed as
Phase 0 — the type, the bus, `subscribe`, `Filter`, and the `Lag` recovery
helper — deliberately with no consumers wired.

**It still has none.** A repo-wide grep for `HarnessEvent` outside
`crates/trusty-agents-common/src/events/` returns exactly three hits, all doc
comments in `crates/trusty-code/src/events.rs` explaining why that crate
diverges from the convention. No crate produces a `HarnessEvent` and no crate
consumes one. Epic #3157 records the same gap from the bus side: promote the
implementation into `trusty-common::control_bus`, add durability and replay, and
stop the harnesses retaining competing buses. `crates/trusty-common/src/` has no
`control_bus` module today, so none of that has landed either.

The console side is the same story from the other end. `trusty-console` polls
each service's `console_metrics` over a supervised stdio MCP child every 15
seconds and caches the last report
(`crates/trusty-console/src/metrics_poller.rs`). That is a gauge transport: it
answers "what is true now", and it cannot answer "what happened, in order". A
grep for `text/event-stream`, `EventSource`, or any SSE handler across
`crates/trusty-console/src` and `crates/trusty-console/ui/src` returns nothing.
Console cannot show a sequence at all today.

So the dashboard the owner asked for is not a new tab. It needs the consumer half
of ADR-0005 wired, one bus service that all three harnesses feed, and a transport
from that bus to a browser that console does not currently have.

---

## {#SPEC-UNIDASH-01~draft} 1. Scope

**Where the dashboard lives: `trusty-console`.** ADR-0032 rules that no
trusty-\* service binds its own HTTP listener and that console is the one shared
daemon extending a service to the web. ADR-0035 adds that console aggregates over
UDS and clients read console rather than dialing each daemon. #6155 is the
standing owner direction applying that to UI specifically: the embedded tool UIs
in search, memory, and analyze migrate into console, and the bundles are removed
only after console absorbs the function. A per-crate dashboard would be a fourth
embedded UI to migrate later. The dashboard is a console surface from the first
commit.

**What ships.** Three routes and one service.

| Surface | What it is |
|---|---|
| `/ui/stream/list` | The sequential list view, non-interactive |
| `/ui/stream/tree` | The call-graph tree view, non-interactive |
| `/ui/object/<type>/<id>` | The read-only object viewer, one route per object type |
| the event bus | One service ingesting from all three harnesses (§4) |

The screensaver is not a fourth route. It is a mode of the two view routes,
described in §5.3, built on the `/ui/screensaver` route #6519 landed.

**What this spec does not change.**

- **The Overview tab and machine-status pane stay as they are.** #6516 phase 2
  built them on the 15-second pull path, and `MachineStatusPanel.svelte` remains
  a gauge view. The stream views sit beside it, reading a different transport.
- **No harness loses its native event enum.** `trusty-agents::events::Event`,
  `trusty-code::events::Event`, and `trusty-mpm::control::event::SessionEvent`
  each keep their taxonomy and their in-process consumers. Each gains an adapter
  that also publishes the unified envelope. ADR-0005's "adapt, don't fold" rule
  is the reason: folding three taxonomies into one mega-enum makes every new
  variant a cross-crate change.
- **The views never command anything.** They display. The directive says
  "non interactive" and this spec takes it literally — no click handler, no
  form, no control affordance on either view. The object viewer is a separate
  route reached by a link, and it is read-only too (§6).
- **The dashboard is not an alerting system.** It shows what happened. Deciding
  that something is wrong, and telling somebody, is out of scope.

**Interactivity, stated precisely.** "Non-interactive" constrains the *view's own
affordances*, not the browser's. Scrolling, following a link, and the browser's
own zoom stay available, because none of them is a control the view offers. What
the view must not have is a filter widget, a play/pause button, a node the user
drags, or a tree the user expands by hand. Filters are set in the URL (§5.1) so a
wall display can be pointed at a filtered stream with no one present to configure
it.

---

## {#SPEC-UNIDASH-02~draft} 2. User stories

Four personas. Each story names what the persona sees in the list, what they see
in the tree, and what they click through to.

### 2.1 The mpm PM operator

*I am running five sessions across three projects, and I want to know which agent
is doing what right now.*

- **List.** One row per event across every live `tm` session, newest at the
  bottom, filtered to `source=mpm`. A row reads: timestamp, session name, actor,
  kind, and a one-line object summary — "14:03:12 · tm-trusty-tools-02 ·
  rust-engineer · workflow.spawn · code-critic". A dispatch, a worktree creation,
  a hook firing, and a session ending are all rows in the same column.
- **Tree.** The PM session is the root. Each dispatched agent is a child node
  that appears when its `Workflow::Spawn` arrives and stays lit until its
  `Workflow::Stop`. Under an agent, its tool calls and file writes are leaves.
  The operator sees at a glance that four agents are live, one has been running
  for eleven minutes with no leaf activity, and one finished.
- **Click-through.** The agent node links to the session viewer; the file leaf
  links to the diff viewer; a `Workflow::Spawn` node links to the delegation's
  prompt and the worktree path.

*What this replaces:* reading five tmux panes, or `tm session list` on a loop.

### 2.2 The trusty-code user

*I asked for a refactor and the turn has been running for two minutes. What is it
doing?*

- **List.** One row per event in one `trusty-code` session, filtered to
  `session=<id>`. The rows are the turn's actual work: each `ToolStarted` and its
  matching `ToolFinished` correlated by `call_id`, each file read, each file
  write, each inference request and its token counts.
- **Tree.** The turn is the root. Sub-agent delegations branch under it, and
  each tool call is a leaf under whichever agent made it — `agent_id` is the
  correlation key, because `agent` alone cannot separate two concurrent
  `python-engineer` delegations (`crates/trusty-code/src/events.rs`, the
  `ToolStarted` doc comment). A leaf that is still open renders active, so a tool
  call hanging for ninety seconds is visible as the one node still lit.
- **Click-through.** A tool-call leaf links to the tool-call viewer showing
  arguments and result; a file leaf links to the diff; an inference node links to
  the request's model, token counts, and latency.

*What this replaces:* watching a scrolling transcript and inferring structure
from indentation.

### 2.3 The trusty-agents operator

*Izzie has been processing eventstream traffic overnight. What did she act on?*

- **List.** One row per event across the assistant's tasks and workstreams,
  filtered to `source=agents`. Rows include listener deliveries, task creation,
  each sub-agent activation, and each memory write. The list reads as an
  overnight log.
- **Tree.** Each task is a root, so the view is a forest rather than a single
  tree. Under a task: the activation that started it, the sub-agents it spawned,
  the tools they called. A workstream spanning several activations shows as
  several roots sharing a workstream id, which the tree groups but does not
  merge — DOC-52 makes a workstream a container of many tasks, not a single call.
- **Click-through.** The task node links to the task viewer; the listener node
  links to the source event that triggered it; a memory write links to the stored
  note.

*What this replaces:* reading the interaction log after the fact.

### 2.4 The wall display

*A monitor in the corner of the room shows the machine working, and nobody is
sitting at it.*

- It shows the same two views, unfiltered, rotating between them on a fixed
  cadence.
- It never prompts, never waits for input, and never shows an error dialog. A
  daemon that goes away is a degraded badge, not a modal.
- It renders correctly with zero events — an empty stream is a legitimate state,
  not an error (§8.5).
- It is what phase 4 of #6516 wraps in a macOS `.saver` bundle, so it inherits
  that constraint: correct for hours with no observer, and no state that grows
  without bound.

*What this replaces:* nothing. It is the reason the directive says "screensaver
too".

---

## {#SPEC-UNIDASH-03~draft} 3. Event taxonomy

### 3.1 The envelope is ADR-0005's, with two additions

`HarnessEvent` already carries `{source, session, seq, at, payload}`
(`crates/trusty-agents-common/src/events/bus.rs:121`). The dashboard needs two
fields that envelope does not have, and one payload arm.

```rust
pub struct HarnessEvent {
    pub source: HarnessSource,          // existing: Agents | Mpm | Code
    pub session: Option<String>,        // existing
    pub seq: u64,                       // existing: process-monotonic
    pub at: DateTime<Utc>,              // existing
    pub payload: HarnessPayload,        // existing: Lifecycle | Hook | Ping
    // added by this spec:
    pub id: EventId,                    // globally unique, stable across relay
    pub parent_id: Option<EventId>,     // the call-graph edge; None = a root
}
```

- **`id`** is a UUIDv7 minted by the emitting process. It is unique across
  harnesses and stable through every relay hop, so the tree can be assembled from
  events that arrive out of order and over different transports. `seq` cannot
  serve this purpose: it is process-monotonic in `trusty-agents-common`, and
  per-session in `trusty-code`, so two processes produce colliding values.
- **`parent_id`** is the whole call graph. A node's parent is the event that
  *caused* it, not the event that preceded it. A tool call's parent is the agent
  spawn that owns the loop making the call; an agent spawn's parent is the
  workflow start of the session that dispatched it. `None` marks a root.
- **The new payload arm** is `HarnessPayload::Action(ActionEvent)`, added
  alongside `Lifecycle`, `Hook`, and `Ping`. It serializes as
  `{"domain":"action","event":{…}}`, matching the existing
  `#[serde(tag = "domain", content = "event")]` shape, so an old subscriber that
  matches on domain skips it cleanly rather than failing to deserialize.

### 3.2 The typed kinds

`ActionEvent` carries the directive's taxonomy. Each variant is one thing that
happened, and each names its object by reference rather than by value.

```rust
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ActionEvent {
    Workflow { phase: WorkflowPhase, actor: Actor, object: ObjectRef, .. },
    Agent    { phase: AgentPhase,    actor: Actor, agent_id: String, .. },
    File     { phase: FilePhase,     actor: Actor, path: PathRef, .. },
    Tool     { phase: CallPhase,     actor: Actor, tool: String, call_id: String, .. },
    Session  { phase: SessionPhase,  actor: Actor, .. },
    Inference{ phase: CallPhase,     actor: Actor, model: String, .. },
}
```

| Kind | Phases | What it means |
|---|---|---|
| `Workflow` | `Start`, `Stop`, `Spawn`, `Read`, `Write` | The directive's five, verbatim. `Spawn` opens a tree node; `Start`/`Stop` bracket a unit of work; `Read`/`Write` are the workflow's own state transitions, not file I/O |
| `Agent` | `Spawned`, `Message`, `Done`, `Failed` | An agent's life. Maps 1:1 onto `LifecycleEvent`'s `AgentSpawned`/`AgentMessage`/`AgentDone`/`AgentFailed` |
| `File` | `Read`, `Written`, `Created`, `Deleted`, `Moved` | Filesystem effects. Carries a repo-relative path and, for `Written`, a diff reference |
| `Tool` | `Started`, `Finished`, `Errored` | One tool invocation, correlated by `call_id` |
| `Session` | `Started`, `StatusChanged`, `Input`, `Done`, `Cancelled` | Session lifecycle |
| `Inference` | `Started`, `Finished`, `Errored` | One model call, with model id, token counts, and latency on `Finished` |

`Tool` and `Inference` are included because the sources already emit them and the
tree is unreadable without them: a coding turn that shows agent spawns but not
tool calls has no leaves. `Session` is included because it is the tree's root
event.

**Required fields on every `ActionEvent`.**

| Field | Type | Why the view needs it |
|---|---|---|
| `id` | `EventId` | Node identity; link target |
| `at` | `DateTime<Utc>` | List ordering; duration arithmetic |
| `source` | `HarnessSource` | Filter axis; color band |
| `session` | `Option<String>` | Filter axis; tree partition |
| `parent_id` | `Option<EventId>` | The call-graph edge |
| `actor` | `Actor` | Who did it — an agent name plus its stable `agent_id`, or `Operator`, or `System` |
| `objects` | `Vec<ObjectRef>` | What it acted on; each entry is one link target for §6 |

`ObjectRef` is `{ type, id, label }` — a discriminated type tag, an identifier
opaque to the view, and a short display label. The view renders `label` and links
on `(type, id)`. It never inlines object content, so an event stays small no
matter how large the thing it names.

### 3.3 Versioning

The `ActionEvent` schema carries `schema_version: u16`, starting at 1, on the
payload rather than the envelope. Three rules make it survivable.

1. **Additive within a version.** A new field is added with `#[serde(default)]`;
   a new `kind` or `phase` variant is added without renumbering. Neither bumps
   the version.
2. **An unknown `kind` renders as a generic row.** The list shows it with its
   `actor` and `at`; the tree attaches it to its `parent_id` and draws it as an
   unlabelled leaf. A consumer never drops an event it does not understand,
   because dropping it silently breaks the tree below it.
3. **A version bump is a breaking field change** — a removal, a retype, or a
   changed meaning. The bus rejects a payload whose `schema_version` exceeds what
   it knows and reports the count, so an upgrade skew is visible rather than
   silent.

`HarnessPayload`'s existing `domain` tag is untouched, so this versioning applies
only inside the new `Action` arm.

### 3.4 What already fits, and what needs an adapter

**Fits with a mechanical mapping.**

- `trusty-agents`' `Event` (`crates/trusty-agents/src/events.rs`) covers
  `SessionStarted`/`SessionDone`/`SessionCancelled`, `AgentSpawned`/`AgentMessage`/
  `AgentDone`/`AgentFailed`, `ToolCalled`/`ToolResult`, and `LlmRequested`/
  `LlmResponded`. Those map onto `Session`, `Agent`, `Tool`, and `Inference`
  directly. `LifecycleEvent` in `trusty-agents-common` is the same set minus
  `Ping`, so half the mapping is already written.
- `trusty-code`'s `ToolStarted`/`ToolFinished`/`ToolError` carry `call_id`,
  `agent`, and `agent_id` and map onto `Tool` with no loss.
- `trusty-mpm`'s `SessionEvent::Started`/`Restarted`/`AuthFailed` map onto
  `Session`.

**Needs an adapter, and why.**

| Source | The gap | The adapter |
|---|---|---|
| all three | No `parent_id` exists anywhere today. Every enum is a flat stream | Each emitter threads a causal context — the current agent's spawn event id — through its call path and stamps it. This is the largest single piece of work in the spec, and it is per-crate |
| `trusty-code` | `seq` is per-session, assigned by `SessionRegistry`, and the wire shape of `session.attach` is shipped (#2054) | The adapter publishes a second, additive `HarnessEvent` alongside the existing envelope. `SessionEventEnvelope` is not changed; #2054's contract is not broken |
| `trusty-mpm` | Hooks are `{kind, data: Value}` with Claude Code's PascalCase names (`crates/trusty-mpm/src/core/hook.rs:25`), deliberately untyped per ADR-0005 | A projection maps the hooks that have an obvious kind — `PreToolUse`/`PostToolUse` to `Tool`, `SessionStart`/`SessionEnd` to `Session`, `FileChanged` to `File`, `WorktreeCreate`/`WorktreeRemove` to `Workflow` — and leaves the rest on the existing `Hook` domain. The untyped arm stays; the projection is additive |
| `trusty-mpm` | `SessionEvent::Output` carries raw backend bytes, which is a transcript, not an action | Not projected. The stream is actions, not output. The transcript stays reachable through the session viewer (§6) |
| `trusty-agents` | Tasks and workstreams have no lifecycle events at all; DOC-54 §2.3 makes eventstream processing the primary path, but the code emits nothing at task granularity | New emissions, not a mapping. `Workflow::Start`/`Stop` per task, and a workstream id on the envelope so the tree can group roots |
| `trusty-analyze` | Publishes on the DOC-72 ring, not on this bus | None needed. DOC-72 §4 already mirrors `HarnessEvent`'s field names by hand and drains through a console cursor; §4.4 below folds that path in as-is |

**Nothing about `File` fits today.** No harness emits a typed file event. `trusty-mpm`
has a `FileChanged` hook, which reports that a watched file changed, not that an
agent wrote it. Every `File` variant is a new emission at the tool boundary.

---

## {#SPEC-UNIDASH-04~draft} 4. The event bus service

### 4.1 Where it lives

Three placements were considered.

| Option | What it means | Why not |
|---|---|---|
| **A. Console-hosted** | Console owns the bus; the harnesses relay to it | Console becomes a dependency of every harness's telemetry. A harness running with console stopped loses its events entirely, and `tm` must run without console |
| **B. A new crate** | `trusty-eventbus`, a fourth daemon | ADR-0032 says no new service binds HTTP, and a UDS-only fourth daemon adds a supervision target, an install step, and a failure mode for no capability the existing crates lack. #3157 explicitly wants *fewer* bus implementations |
| **C. `trusty-common::control_bus`** | The bus promoted into the common layer, as epic #3157 already scoped | — |

**Recommendation: C.** The name is `control_bus`, in `trusty-common`, exactly as
#3157 specifies. Three reasons.

1. **The decision is already made.** #3157's consolidated scope opens with
   "promote the existing events implementation into
   `trusty-common::control_bus`". This spec does not get to re-decide it; it
   supplies the consumer that makes the promotion worth doing.
2. **`trusty-common` is the one crate all three harnesses already depend on.** A
   bus there needs no new Cargo edge in any direction. `trusty-agents-common` is
   the wrong home for the same reason DOC-72 §4 avoids it: `trusty-analyze` takes
   no edge on `trusty-agents-common` and mirrors the envelope by field name to
   stay uncoupled. The common layer is where a shared type stops needing that
   workaround.
3. **A library, not a daemon.** The bus is an in-process broadcast plus a
   durable log plus a UDS relay. It runs inside whichever process emits, exactly
   as ADR-0005's bus does today. Nothing new gets supervised.

`trusty-agents-common::events` stays where it is and re-exports from
`control_bus`, so no consumer of the current API breaks. #3157's "harnesses do
not retain competing event-bus implementations" acceptance criterion is met by
migration, not by deletion in this spec's phases.

### 4.2 Ingestion

Three paths, one envelope.

- **In-process publish.** Code inside a harness calls
  `control_bus::publish(event)`, which stamps `id`, `seq`, and `at` and
  broadcasts. This is the existing `publish` (`bus.rs:248`), extended with `id`
  and `parent_id`.
- **Child-to-parent stderr relay.** A subprocess writes one NDJSON line prefixed
  `__OMPM_EVENT__ ` and the parent re-publishes it. This already works
  (`EVENT_LINE_PREFIX`, `bus.rs:53`) and is how a `--workflow` child reaches its
  API server today. It stays the transport for anything the harness spawns.
- **UDS relay to console.** Each harness exposes a cursor method on the UDS
  socket it already serves, and console drains it. This is DOC-72 §4's console
  relay generalized from one daemon to three:

  ```
  <service>.bus_events  { since_seq, max }
    -> { events: [ … ], next_seq, dropped }
  ```

  The method name differs per service, the shape does not. ADR-0032 holds: no
  harness binds HTTP, console is the only HTTP surface, and console reaches each
  service over UDS as ADR-0035 directs.

### 4.3 Ordering, retention, and backpressure

**Ordering.** `seq` is monotonic *within one process*, and nothing makes it
monotonic across processes. The bus therefore defines two orderings and the views
use them for different things.

- **Causal order** comes from `parent_id`. It is total within a subtree and is
  the only ordering the tree view uses.
- **Display order** is `(at, source, seq)`, applied by the reader, not the
  producer. The list view uses it. Two events from different harnesses with equal
  `at` order by `source` then `seq`, deterministically, so two viewers of the
  same stream show the same sequence.

Clock skew between processes on one machine is bounded by the OS clock, so `at`
is sufficient here. A cross-machine bus would need a different answer, and this
spec does not have one (§10 Q3).

**Retention.** Two tiers.

- **The ring.** An in-memory `broadcast` channel, capacity 1024 today
  (`CHANNEL_CAPACITY`, `bus.rs:64`). Live subscribers read this. Overflow is
  reported, never silent — ADR-0005's `Lag { skipped }` (`bus.rs:146`) already
  translates `RecvError::Lagged(n)` into a typed notice and resumes the stream.
  The dashboard renders a lag notice as a visible gap marker in both views.
- **The log.** A durable NDJSON file per day, rotated and retained per #3157's
  "durable JSONL logging, rotation, retention, and replay". A viewer that opens
  mid-session reads the log to build the tree it missed, then switches to the
  live ring at the log's last `seq`. Without this, the tree view is empty until
  the next spawn and a wall display shows nothing after a console restart.

**Backpressure.** The bus never blocks a producer. `publish` is best-effort
already, ignoring `SendError` when no subscriber exists. A slow reader lags and
gets a `Lag` notice; it does not slow the harness down. The rule is explicit
because the alternative — a full channel stalling an agent's tool call — would
make telemetry able to break the work it observes.

**Overflow, three defenses.**

1. Capacity rises from 1024 to a configurable value, defaulting to 8192 for a
   bus carrying three harnesses' actions rather than one harness's lifecycle.
2. The console reader drains on a cursor, so a browser that stops reading does
   not lag the bus — only console's own SSE buffer.
3. The log is written from the producer side, so a lagged reader loses nothing
   permanently. It re-reads.

### 4.4 Fan-out to viewers

Console is the only HTTP surface, so every viewer reads console.

| Route | Shape | Consumer |
|---|---|---|
| `GET /api/console/events/stream?since_seq=<n>&<filters>` | JSON `{events, next_seq, dropped}` | A programmatic reader, and the browser's initial backfill |
| `GET /api/console/events/stream/sse?<filters>` | SSE, one message per event | The two dashboard views |

**SSE, not WebSocket.** The stream is one-directional by construction — the views
are non-interactive and send nothing back. SSE reconnects on its own with
`Last-Event-ID`, which maps exactly onto `since_seq`, and it needs no new
dependency in the axum server console already runs. A WebSocket buys a return
channel this spec has no use for. The existing `console_metrics` polling loop
(`crates/trusty-console/src/metrics_poller.rs`) is unchanged and keeps serving
the gauge routes.

Filters apply server-side on the query string — `source`, `session`, `kind`,
`actor` — using the same predicate as `events::Filter` (`bus.rs`, `filter.rs`),
extended with the `kind` and `actor` axes. Server-side filtering is what lets a
wall display subscribe to one session without shipping every event to it.

**DOC-72's analyze events ride this bus unchanged.** DOC-72 §4 already publishes
LSP results as `HarnessEvent`-shaped payloads into a per-`(workspace, language)`
ring, drained by `analyze.lsp_events { since_seq, max } -> {events, next_seq,
dropped}` from `metrics_poller.rs` and republished at
`/api/console/events/analyze/lsp`. That is this section's ingestion path and
fan-out path, one service early. Two reconciliations:

- The analyze payloads become `ActionEvent::Tool` with `tool: "lsp.<method>"`,
  so they appear as leaves in the tree beside every other tool call rather than
  in a parallel feed.
- `/api/console/events/analyze/lsp` stays as a service-specific route for a
  programmatic reader, and its events also appear on the unified
  `/api/console/events/stream`. The dashboard reads only the unified route.

---

## {#SPEC-UNIDASH-05~draft} 5. The views

Both views are Svelte 5 components in `crates/trusty-console/ui/src/`, built by
Vite and embedded by `rust_embed` through `console_ui.rs`. The console UI is a
Vite SPA, not SvelteKit (`crates/trusty-console/ui/package.json`), so "route"
here means a path the SPA shell resolves, exactly as `/ui/screensaver` already
does. Decision logic goes in plain `.js` modules tested by `node --test
src/*.test.js`, following the split `screensaver.js` established: the module is
pure functions of its inputs, the `.svelte` file is a renderer over them.

### 5.1 List view — `/ui/stream/list`

Sequential, newest last, scrolling.

- **Virtualized.** Only the visible rows exist in the DOM. A day-long session
  produces tens of thousands of events and an unvirtualized list stops scrolling
  smoothly in the low thousands.
- **A bounded window.** The client holds at most `N` events in memory (default
  20 000, §8.2) and drops from the head. Older events are reachable through the
  cursor route, not by scrolling forever.
- **Follow-tail by default.** The view pins to the newest row. A wall display
  never needs to be scrolled to stay current.
- **Filters live in the URL**, not in a widget:
  `?source=mpm&session=<id>&kind=file&actor=rust-engineer`. Absent means
  unfiltered. Combining axes ANDs them, matching `events::Filter`'s existing
  semantics.
- **A row is:** timestamp (mono), source badge, session, actor, kind badge, and
  the first `ObjectRef`'s label as a link. Foundry's typographic split applies —
  IBM Plex Mono for every identifier, path, count, and status, IBM Plex Sans for
  the label (DOC-39 §8).
- **Gap markers.** A `Lag { skipped }` renders as a full-width mono rule reading
  the skipped count. It is never omitted.

### 5.2 Tree view — `/ui/stream/tree`

A call graph, keyed on `parent_id`.

**Node lifecycle.** A node appears when its opening event arrives and stays
*active* until its closing event does.

| Opens the node | Closes it |
|---|---|
| `Workflow::Start`, `Workflow::Spawn` | `Workflow::Stop` with the same `id` as `parent_id` |
| `Agent::Spawned` | `Agent::Done` or `Agent::Failed` |
| `Tool::Started` | `Tool::Finished` or `Tool::Errored`, correlated by `call_id` |
| `Inference::Started` | `Inference::Finished` or `Inference::Errored` |
| `Session::Started` | `Session::Done` or `Session::Cancelled` |

A `File` event has no duration and is drawn as a point leaf, present from arrival.

**Active means visibly active.** An open node carries Foundry's `--accent-soft`
amber treatment and a running duration in mono. A closed node takes green for
success or red for failure (DOC-39 §8's role mapping — green is done, red is
blocked or failed). The wall display's whole value is that a stuck node is the
one still amber after ten minutes, so the active state is the view's most
important visual signal and must not be subtle.

**A node that never closes.** After a configurable stall threshold (default 10
minutes) an active node adds a stalled marker. It does not close on its own,
because a closing event that never arrives is exactly the condition worth seeing.
A node whose root session ends is force-closed as `orphaned` when the session's
`Session::Done` arrives, so a crashed harness does not leave the tree amber
forever.

**Layout.** Top-down tidy tree, computed by `d3-hierarchy`'s `tree()` (§7).

- Children are ordered by their opening event's `at`, left to right, so the
  horizontal axis reads as time within a level.
- A new child is inserted at its position and the subtree reflows. Reflow is
  animated over 300ms with an ease-out curve; a node's entry fades and scales
  from 0.9 over 150ms. Nothing else animates. The rule is that motion means a
  change happened, so idle time is completely still — a screensaver that jitters
  while nothing is happening is worse than a static image.
- Depth is capped for display at 8 levels. Deeper subtrees collapse to a count
  badge on the deepest visible node.

**Collapse of finished subtrees.** A subtree whose root closed more than 60
seconds ago collapses to a single summary node — the root's label, its duration,
its terminal status, and a child count. It stays collapsed. This is what keeps a
day-long session's tree bounded: the tree shows what is happening now at full
detail and what happened earlier as one node per finished unit of work. Collapse
is automatic and time-based, never a click, because the view is non-interactive.

**Forest, not tree.** The trusty-agents case (§2.3) has many roots. The view
lays out each root as its own tree, stacked vertically, and scrolls. A root that
has been collapsed and has no active descendant scrolls off the top.

### 5.3 Screensaver mode

The screensaver is a mode of §5.1 and §5.2, not a third view. It builds on
`/ui/screensaver` (#6519, phase 3 of #6516) and reuses that route's decision
layer in `crates/trusty-console/ui/src/screensaver.js`, which is already pure and
test-covered without a browser.

- **Rotation.** The screensaver cycles list → tree → machine-status pane, on the
  existing rotation cadence. The list and tree join the rotation set; the
  rotation mechanism is unchanged.
- **Idle entry.** Unchanged. `screensaver.js`'s `IDLE_STORAGE_KEY`
  (`trusty-console-screensaver-idle-minutes`) still governs whether the console
  navigates to the screensaver on its own, and it stays opt-in with no settings
  UI.
- **Poll cadence does not apply.** The screensaver's existing `POLL_BASE_MS`
  (15 s) and `POLL_CAP_MS` (60 s) backoff governs the gauge panes. The stream
  views are SSE-driven and have their own reconnect (§8.1). A disconnected SSE
  shows a degraded badge and retries with the same backoff shape.
- **Any gesture exits**, per the existing `IDLE_EVENTS` handling, returning to
  `/ui`.
- **macOS `.saver` compatibility.** Phase 4 of #6516 loads
  `http://127.0.0.1:7788/ui/screensaver` in a WKWebView. Both stream views must
  therefore be correct with no observer for hours, which is what §5.2's collapse
  rule and §8.2's memory ceiling exist to guarantee.

---

## {#SPEC-UNIDASH-06~draft} 6. The object viewer

Every `ObjectRef` on an event is a link. The link target is one route per object
type, and every one of them is read-only.

`/ui/object/<type>/<id>`

| Type | What it shows | Where the data comes from |
|---|---|---|
| `session` | Status, harness, project, worktree, start and end, the event count, and the transcript | The owning harness's session record over UDS |
| `agent` | Agent name, `agent_id`, the delegation prompt, the dispatching session, and the report | The spawn event's payload plus the harness's agent record |
| `task` | Task title, state, workstream, and its activations | trusty-agents' task store |
| `workstream` | The workstream and every task under it | trusty-agents; DOC-52 is authoritative for the term |
| `file` | The path, and for a `File::Written` the unified diff | The diff reference on the event; the repo for context |
| `tool_call` | Tool name, `call_id`, arguments, result or error, and duration | The `Tool::Started`/`Finished` pair |
| `inference` | Model, token counts, latency, and stop reason | The `Inference` pair |
| `issue` / `pr` | Number, title, state, and a link out to GitHub | `gh`, through the harness that referenced it |

**Rules that apply to all of them.**

- **Read-only.** No route mutates anything. There is no "cancel this session"
  button, no "retry", no edit. The dashboard observes.
- **Reached by link only.** The stream views do not embed object content. This is
  what keeps an event small (§3.2) and keeps the two views renderable at
  thousands of nodes.
- **Opens in a new context.** Following a link from a non-interactive view must
  not navigate the view away, or a wall display would be one stray click from
  showing a diff forever. The link opens in a new tab; the screensaver mode
  suppresses links entirely.
- **A missing object is a stated absence, not an error.** A session pruned since
  the event was emitted renders "no longer available" with the id. A viewer that
  cannot reach the owning daemon names that daemon instead, and never claims the
  object is gone — the two conditions look identical to the viewer and mean
  different things to the reader.

---

## {#SPEC-UNIDASH-07~draft} 7. Visualization library

### 7.1 What the tree actually needs

The directive asks for "a fast stylish viz library". The requirement underneath
it is narrower than a general graph library, and naming that narrows the
candidate set sharply.

- The data is a **rooted forest**, not a general graph. Every node has exactly
  one `parent_id`. There are no cycles and no cross-edges.
- The view is **non-interactive** (§1). No dragging, no click-to-expand, no
  physics the user perturbs.
- The layout is **incremental**: nodes arrive one at a time and the tree reflows.
- Nodes carry **Foundry styling** — tokens, mono/sans split, badge shapes
  (DOC-39 §8) — and hardcoded colors are a spec violation there.
- The target is **~2000 visible nodes** with 300ms reflow (§8.1).

### 7.2 Candidates

| Library | Licence | Approx. min+gz | Renderer | Verdict |
|---|---|---|---|---|
| `d3-hierarchy` + custom canvas | ISC | ~5 KB (plus `d3-shape`/`d3-transition` if used) | Ours | **Recommended.** Layout only. It computes coordinates for a hierarchy and hands them back. Rendering, theming, and animation stay in our code, so Foundry tokens apply directly with no library theme to override |
| `graphology` + `sigma.js` | MIT | ~90 KB | WebGL | **Fallback.** Genuinely fast at tens of thousands of nodes. Overkill for a forest, and its node rendering is a WebGL program, so Foundry tokens reach it only by being read out of CSS and passed in as uniforms |
| `cytoscape.js` | MIT | ~350 KB | Canvas | No. Built around interaction — selection, panning, gestures, an event model — all of which §1 forbids. Its tree layouts are extensions, and its styling is its own selector language rather than CSS |
| `@xyflow/svelte` | MIT (core; Pro features paid) | ~60 KB + Svelte peer | DOM | No. DOM nodes per graph node caps out in the low hundreds, which is under the §8.1 target. It is a node-editor library, and this is not an editor |
| `vis-network` | MIT / Apache-2.0 | ~180 KB | Canvas | No. Physics-based layout by default, which is exactly the jitter §5.2 forbids. Maintenance has been intermittent since the visjs org split |

For the list, a virtualizer rather than a chart library.

| Library | Licence | Approx. min+gz | Verdict |
|---|---|---|---|
| `@tanstack/svelte-virtual` | MIT | ~6 KB | **Recommended.** Svelte 5 support is current, the API is a headless hook returning offsets, and it imposes no markup or styling |
| `svelte-virtual` | MIT | ~4 KB | Fallback. Smaller, but a smaller maintainer base and a component API that owns the row markup, which fights Foundry's row styling |

### 7.3 Recommendation

**`d3-hierarchy` for layout plus a hand-written canvas renderer for the tree, and
`@tanstack/svelte-virtual` for the list.** The reason is that every general graph
library on the list pays its bundle and its complexity for two things this view
does not have — arbitrary graph topology and user interaction — while giving up
the one thing it does need, which is that Foundry tokens style the nodes
directly. `d3-hierarchy` is a layout function, not a framework: it takes a
parent-keyed array, returns coordinates, and never touches the DOM. Canvas holds
2000 nodes at 60fps without a WebGL dependency, and a canvas renderer reads
Foundry's CSS custom properties at draw time, so light and dark themes work with
no library-specific theming layer.

**Fallback: `graphology` + `sigma.js`.** The trigger is measured, not
speculative: if the phase-2 acceptance run (§9) cannot hold 300ms reflow at 2000
nodes on the canvas renderer, switch the tree to sigma's WebGL renderer and
accept that node styling moves from CSS into shader uniforms. The layout code
does not change, because `graphology` accepts coordinates computed by
`d3-hierarchy`.

**Rejected outright: any library that ships an interaction model.** Cytoscape,
xyflow, and vis-network all do. Adopting one and then disabling its interaction
is more work than not adopting it, and it leaves a live event handler on a route
that is supposed to have none.

---

## {#SPEC-UNIDASH-08~draft} 8. Non-functional requirements

### 8.1 Latency, emit to pixel

The budget is **500 ms at p95**, from `control_bus::publish` returning in the
harness to the row or node being painted.

| Hop | Budget | Note |
|---|---|---|
| `publish` to the harness's ring | < 1 ms | An in-process broadcast send |
| Ring to console, over UDS | ≤ 250 ms | Cursor drain interval. This is the dominant term and the one to tune |
| Console to browser, over SSE | < 20 ms | Loopback |
| Browser parse, insert, paint | ≤ 100 ms | Includes the tree's 300 ms reflow animation, which starts inside the budget and finishes outside it |

The 250 ms drain replaces `metrics_poller.rs`'s 15-second cadence for this path
only. The gauge routes keep their 15 seconds — a metrics snapshot is not worth 60
polls a minute.

**SSE reconnect.** On disconnect the browser retries with the same backoff shape
`screensaver.js` uses for polling — full speed for three attempts, then backing
off toward a 60-second cap — and resumes from `Last-Event-ID` so no event is
missed and none is duplicated.

### 8.2 Memory, over a day-long session

| Component | Ceiling | Mechanism |
|---|---|---|
| Harness ring | 8192 events | Fixed capacity; overflow reports `Lag` (§4.3) |
| Console SSE buffer, per connection | 4096 events | A client slower than this is disconnected and reconnects on its cursor |
| Browser, list view | 20 000 events | Bounded window, dropping from the head |
| Browser, tree view | 2000 live nodes | §5.2's automatic collapse of finished subtrees is what enforces this |
| Durable log | rotated daily, retained per #3157 | Disk, not memory |

**The browser is the binding constraint, and collapse is what makes it hold.** A
busy PM session emits on the order of 10 000 events in a day. Without §5.2's
time-based collapse the tree would grow to that node count and the canvas
renderer would fall out of budget somewhere in the low thousands. With it, the
node count tracks *concurrent* work, which is bounded by the ~5 workstream
attention limit (ADR-0030) rather than by elapsed time.

A `.saver` bundle running for a week must show no growth trend across days. That
is a phase-4 acceptance measurement, not an assertion.

### 8.3 Accessibility

The views are non-interactive, which removes the keyboard-navigation surface and
raises the bar on everything else.

- **Colour is never the only signal.** An active node carries a duration in mono
  as well as amber; a failed node carries a status word as well as red. Foundry's
  red/green pair fails for the most common colour deficiency, so status must be
  legible without it.
- **Contrast meets WCAG AA** (4.5:1 body, 3:1 large) in both themes. Foundry's
  tokens are the source, and any new token pair added for these views is checked
  against both palettes.
- **Motion respects `prefers-reduced-motion`.** With it set, the tree's reflow
  and entry animations are replaced by instant repositioning. Nothing else about
  the view changes.
- **The list is a semantic list**, virtualized rows included, with `aria-live`
  set to `polite` on the tail region so a screen reader announces new events
  without interrupting.
- **The tree canvas carries a text alternative** — a `<figcaption>`-equivalent
  region summarizing live node count, deepest active path, and the oldest active
  node's age. A canvas is opaque to assistive technology, so the summary is the
  view for that reader, and it must be genuinely informative rather than a label.

### 8.4 Theming

Foundry, per DOC-39 §8, with no local exceptions.

- Tokens come from `docs/design/UI/design-system/tokens.css`, already deployed in
  console as `crates/trusty-console/ui/src/foundry.css`.
- Light is default; dark activates via `[data-theme="dark"]` on the root, set
  from `prefers-color-scheme` on init and updated live on OS change (DOC-39
  AC-27.1 through AC-27.3). The existing `ThemeSelector.svelte` and
  `theme.svelte.js` already implement this; the views inherit it.
- **Hardcoded hex is a review failure** (DOC-39 AC-27.5). This binds the canvas
  renderer too: it reads token values from computed style at draw time and
  re-reads them on theme change. A canvas that caches colours at first paint
  silently ignores the theme toggle, which is the specific failure this rule
  exists to prevent.
- Role mapping is Foundry's: rust for the active highlight, green for done, blue
  for in-progress, red for failed, amber for the active-node state.

### 8.5 The zero-event state

**Both views must render correctly with no events, and an empty stream is not an
error.** A fresh console with no harness running, a filter matching nothing, and
a machine at rest are all normal.

- The list shows Foundry's empty state — the muted idle-robot mark, one mono
  label, one line of sans guidance (DOC-39 §8.3). No spinner, because nothing is
  loading.
- The tree shows the same, centred.
- A filter matching nothing says so and names the filter, so a wall display
  pointed at a stale session id is diagnosable from across the room.
- **Distinguish empty from disconnected.** No events and a healthy SSE
  connection is "nothing is happening". No events and a dead connection is
  "cannot see". They render differently, and the second names the daemon.

---

## {#SPEC-UNIDASH-09~draft} 9. Phases and acceptance criteria

Five phases. Each is independently mergeable and each ends in a stated
measurement.

### Phase 1 — The bus and the taxonomy

`trusty-common::control_bus` per #3157, plus `ActionEvent` and the envelope's two
new fields.

**Acceptance.**
1. `control_bus` exists in `trusty-common` with `publish`, `subscribe`, `Filter`,
   and `Lag`, and `trusty-agents-common::events` re-exports from it with no
   consumer change.
2. `HarnessEvent` carries `id` and `parent_id`; a serde round-trip test covers
   every `ActionEvent` variant and phase.
3. An unknown `kind` deserializes to the generic variant rather than failing —
   proven by a test feeding a payload with a `kind` the enum does not have.
4. The durable log writes NDJSON, rotates daily, and replays into an identical
   event sequence.
5. Rung 4: `cargo check --workspace`, then `cargo test -p <consumer>
   --no-fail-fast` for each direct dependent of `trusty-common`.

### Phase 2 — One source end to end, and the list view

`trusty-mpm` emits `ActionEvent`; console drains it and serves it; the list view
renders it.

**Acceptance.**
1. A `tm` session's dispatch, tool calls, and completion appear as rows within
   500 ms p95, measured emit-to-paint over 100 events.
2. `GET /api/console/events/stream` and its SSE sibling both answer, and the SSE
   route resumes from `Last-Event-ID` across a forced disconnect with no gap and
   no duplicate.
3. Every URL filter axis works server-side.
4. The list holds 20 000 events and scrolls at 60fps.
5. The zero-event and disconnected states render distinctly (§8.5).
6. Rung 6: the Rust side at rung 3, plus `node --test src/*.test.js` in
   `crates/trusty-console/ui` and one binary smoke run of the route.

### Phase 3 — `parent_id` and the tree view

Causal context threaded through `trusty-mpm`, then the tree.

**Acceptance.**
1. Every event from a `tm` session has a `parent_id`, and the resulting graph is
   a forest — no cycles, no orphans other than deliberate roots. A test asserts
   this over a recorded session.
2. A node appears within the §8.1 budget of its opening event and stays active
   until its closing event.
3. A node with no closing event is marked stalled after the threshold and is not
   auto-closed.
4. A subtree finished for 60 seconds collapses, and node count tracks concurrent
   work rather than elapsed time — measured over a one-hour session.
5. **The library decision is settled here by measurement:** 2000 nodes, reflow
   within 300 ms, on the canvas renderer. Failing that, §7.3's sigma fallback
   is taken and the result recorded in this section.
6. `prefers-reduced-motion` disables both animations.

### Phase 4 — The other two sources, and the object viewer

`trusty-code` and `trusty-agents` adapters, plus every route in §6.

**Acceptance.**
1. A `trusty-code` turn and a `trusty-agents` task each render in both views,
   from the same unfiltered stream, correctly interleaved by display order.
2. `trusty-code`'s `SessionEventEnvelope` and the `session.attach` wire shape are
   byte-identical to before — proven by that crate's existing tests passing
   unchanged.
3. `trusty-mpm`'s hook projection covers the named hooks and leaves the rest on
   the `Hook` domain.
4. Every `ObjectRef` type in §6 resolves to a route; a pruned object renders a
   stated absence and an unreachable daemon renders differently.
5. No object route mutates anything — asserted by a test that every handler is
   registered on `GET`.

### Phase 5 — Screensaver and the wall display

Rotation, and the measurements that only a long run can produce.

**Acceptance.**
1. The rotation includes list and tree, and any gesture exits to `/ui`.
2. Links are suppressed in screensaver mode.
3. A 24-hour unattended run shows no memory growth trend and no unbounded node
   count.
4. Both views survive a console restart and a harness restart without a reload,
   resuming on their cursors.
5. The `.saver` bundle from #6516 phase 4 loads both views.

---

## {#SPEC-UNIDASH-10~draft} 10. Open questions for the owner

Eight. Each names what changes depending on the answer.

**Q1 — Does the bus go in `trusty-common::control_bus`, as §4.1 recommends?**
#3157 already says so, but that epic has not moved and this spec would be its
first consumer. Confirming it makes phase 1 a promotion; rejecting it means
choosing between a console-hosted bus and a fourth daemon, and phase 1 changes
shape entirely.

**Q2 — Is the durable log in scope for phase 1, or deferred?**
Without it, a viewer that opens mid-session sees an empty tree until the next
spawn, and a console restart loses history. With it, phase 1 grows a rotation and
retention policy. The spec assumes in scope.

**Q3 — Single machine only, for now?**
§4.3's ordering relies on one OS clock. A bus spanning machines needs a different
answer, and this spec does not have one. The assumption is single-machine.

**Q4 — Is 500 ms the right latency budget?**
It is set to be comfortably achievable rather than derived from a requirement. A
tighter budget mainly costs UDS drain frequency; a looser one would let the drain
share the existing poller cadence.

**Q5 — Should `File` events carry the diff, or a reference to it?**
The spec says reference (§3.2), which keeps events small and requires the viewer
to fetch. Inlining would make the stream self-contained and the log much larger.

**Q6 — How much history should a fresh viewer replay?**
The spec has no number. Replaying a full day makes the tree complete and the
first paint slow; replaying an hour is fast and may show a tree with no roots.

**Q7 — Do the stream views belong inside the existing console SPA shell, or as
bare full-page routes?**
Inside the shell they inherit navigation and the theme selector, which are
affordances a non-interactive view is not supposed to have. Bare, they duplicate
the theme bootstrap. The spec assumes bare, with the shell's theme module
imported.

**Q8 — Does `trusty-review` join as a fourth source?**
It is not in the directive and this spec excludes it. Its review verdicts would
fit `Workflow` cleanly if wanted later.

Two things are unsettled but are measurements rather than questions: §7.3's
library choice is confirmed or overturned by phase 3's 2000-node run, and §8.2's
browser ceilings are initial values rather than derived ones.

---

## {#SPEC-UNIDASH-11~draft} 11. Implementation issues to cut

To be filed by `ticketing` once the owner accepts this spec. One line each; each
references #6611.

1. **Owner decisions on §10 Q1–Q8.** Blocks phase 1's shape. First issue to cut.
2. `trusty-common`: create `control_bus` — move the `trusty-agents-common::events`
   implementation, keep the old path as a re-export (§4.1, #3157).
3. `trusty-common`: add `id` (UUIDv7) and `parent_id` to `HarnessEvent`, stamped
   by `publish` (§3.1).
4. `trusty-common`: the `ActionEvent` enum, its six kinds, `Actor`, `ObjectRef`,
   and `schema_version` with the unknown-kind fallback (§3.2, §3.3).
5. `trusty-common`: the durable NDJSON log — write, rotate daily, retain, replay
   into an identical sequence (§4.3).
6. `trusty-common`: raise `CHANNEL_CAPACITY` to a configurable value defaulting
   to 8192 (§4.3).
7. `trusty-mpm`: thread causal context through the dispatch path and stamp
   `parent_id` on every emission (§3.4). The largest single piece of work here.
8. `trusty-mpm`: publish `ActionEvent` for session, agent, tool, and workflow
   transitions, alongside the existing `SessionEvent` (§3.4).
9. `trusty-mpm`: project the typed Claude Code hooks — `PreToolUse`/`PostToolUse`
   to `Tool`, `SessionStart`/`SessionEnd` to `Session`, `FileChanged` to `File`,
   `WorktreeCreate`/`WorktreeRemove` to `Workflow` — leaving the untyped `Hook`
   arm in place (§3.4).
10. `trusty-mpm`: the `bus_events { since_seq, max }` cursor method on its UDS
    socket (§4.2).
11. `trusty-console`: drain each service's `bus_events` cursor on a 250 ms
    interval, beside the existing `console_metrics` poll in `metrics_poller.rs`
    (§4.2, §8.1).
12. `trusty-console`: `GET /api/console/events/stream` — the cursor JSON route,
    with server-side `source`/`session`/`kind`/`actor` filters (§4.4).
13. `trusty-console`: `GET /api/console/events/stream/sse` — SSE fan-out with
    `Last-Event-ID` resume. Console's first SSE route (§4.4).
14. `trusty-console` ui: `/ui/stream/list` — virtualized list on
    `@tanstack/svelte-virtual`, URL filters, follow-tail, bounded window, gap
    markers (§5.1).
15. `trusty-console` ui: `/ui/stream/tree` — `d3-hierarchy` layout plus a canvas
    renderer, node lifecycle, active state, stall marker, timed collapse, forest
    layout (§5.2, §7.3).
16. `trusty-console` ui: the canvas renderer reads Foundry tokens from computed
    style at draw time and re-reads on theme change (§8.4).
17. `trusty-console` ui: `prefers-reduced-motion` disables both tree animations
    (§8.3).
18. `trusty-console` ui: the tree canvas text alternative — live node count,
    deepest active path, oldest active node's age (§8.3).
19. `trusty-console` ui: the zero-event and disconnected states for both views,
    rendered distinctly (§8.5).
20. `trusty-code`: publish `ActionEvent` additively, leaving
    `SessionEventEnvelope` and the `session.attach` wire shape untouched (§3.4).
21. `trusty-code`: thread causal context and stamp `parent_id`, keyed on
    `agent_id` for concurrent same-name delegations (§3.4).
22. `trusty-code`: the `bus_events` cursor method on its UDS socket (§4.2).
23. `trusty-agents`: publish `ActionEvent` from the existing `Event` emission
    sites, and add task and workstream lifecycle events, which do not exist today
    (§3.4).
24. `trusty-agents`: thread causal context and stamp `parent_id` (§3.4).
25. `trusty-agents`: the `bus_events` cursor method on its UDS socket (§4.2).
26. all three harnesses: emit typed `File` events at the tool boundary — read,
    write, create, delete, move. No harness emits these today (§3.4).
27. `trusty-console` ui: the object viewer routes for `session`, `agent`, `task`,
    `workstream`, `file`, `tool_call`, `inference`, `issue`, `pr` — read-only,
    new-tab, stated-absence handling (§6).
28. `trusty-console` ui: screensaver rotation includes list and tree; links
    suppressed in screensaver mode (§5.3).
29. `trusty-analyze`: map the DOC-72 LSP events onto `ActionEvent::Tool` with
    `tool: "lsp.<method>"` so they appear as tree leaves; keep
    `/api/console/events/analyze/lsp` as the service-specific route (§4.4).
30. **Phase 3 acceptance run: 2000 nodes, 300 ms reflow.** Confirms §7.3's canvas
    recommendation or triggers the sigma fallback; record the result in §7.3.
31. **Phase 5 acceptance run: 24 hours unattended.** Memory trend and node count
    over a full day, on the `.saver` bundle (§8.2, #6516 phase 4).






