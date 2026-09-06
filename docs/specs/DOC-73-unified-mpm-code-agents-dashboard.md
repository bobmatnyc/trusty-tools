---
spec_refs:
  - id: SPEC-TCUI-17~draft
    path: docs/specs/trusty-code-harness-ui.md
    anchor: SPEC-TCUI-17~draft
---

# DOC-73 — The Unified mpm/code/agents Dashboard: One Event Stream, List and Tree

**Status:** Draft. Design record only — no code in this PR.
**Spec ID:** `SPEC-UNIDASH-01~draft` … `SPEC-UNIDASH-17~draft`
**Subsystem:** `trusty-console` — the event bus itself (the UDS ingest socket,
`seq` assignment, the ring, the durable log), the dashboard routes, and the SSE
fan-out (`src/routes/`, `src/metrics_poller.rs`, `ui/src/`); `trusty-common` —
the `HarnessEvent` envelope, the `ActionEvent` taxonomy, and the shared
`PushClient` (§4.1, owner ruling 2026-09-05, superseding the prospective
`control_bus` home in #3157); `trusty-agents-common` — the taxonomy's prior
home, retired once its producers push to console (§3.4, §4.1); `trusty-mpm`,
`trusty-code`, `trusty-agents`, `trusty-analyze` — the four event sources and
their adapters.
**Owner:** Bob Matsuoka
**Last-updated:** 2026-09-06
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
epic this spec no longer routes through — closed not-planned 2026-09-02; the
2026-09-05 owner ruling in §4.1 decides bus placement directly),
[#6155](https://github.com/bobmatnyc/trusty-tools/issues/6155) (embedded tool UIs
migrate into console — the precedent that puts this dashboard in console rather
than in each crate),
[#6516](https://github.com/bobmatnyc/trusty-tools/issues/6516) (the
machine-status dashboard epic these views sit beside),
[#6519](https://github.com/bobmatnyc/trusty-tools/issues/6519) (the
`/ui/screensaver` route this spec's screensaver extends),
[#6606](https://github.com/bobmatnyc/trusty-tools/issues/6606) and
[DOC-72](https://github.com/bobmatnyc/trusty-tools/pull/6607) §4 (the analyze-side
console relay — the first feeder into the bus this spec generalizes), and, for
§§12–17's amendment (owner rulings of 2026-09-06 on console/dashboard
unification), epic [#6922](https://github.com/bobmatnyc/trusty-tools/issues/6922)
and its children
[#6923](https://github.com/bobmatnyc/trusty-tools/issues/6923),
[#6924](https://github.com/bobmatnyc/trusty-tools/issues/6924),
[#6925](https://github.com/bobmatnyc/trusty-tools/issues/6925),
[#6926](https://github.com/bobmatnyc/trusty-tools/issues/6926),
[#6927](https://github.com/bobmatnyc/trusty-tools/issues/6927),
[#6928](https://github.com/bobmatnyc/trusty-tools/issues/6928),
[#6929](https://github.com/bobmatnyc/trusty-tools/issues/6929),
[#6930](https://github.com/bobmatnyc/trusty-tools/issues/6930), and
[#6931](https://github.com/bobmatnyc/trusty-tools/issues/6931) (§17 orders all
ten).

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

**Navigation and scope (owner ruling, 2026-09-06).**

> "the mpm dashboard should be accessed from the MPM Sessions console, and the
> list/tree dashboard visualizer should link from the session list. Dashboards
> should be per session."
> — Bob, 2026-09-06

1. **Entry point.** The mpm dashboard is reached from the MPM Sessions
   console — the trusty-console Sessions screen (`SessionsTab.svelte`). It is
   not a separate top-level destination.
2. **Linking.** Each row in the session list links to that session's
   list/tree dashboard visualizer.
3. **Scope.** A dashboard is per session. One managed session has one
   dashboard. There is no global dashboard across sessions.

**Acceptance test.** A dashboard design with no per-session route, or with no
link from the session list, fails this ruling.

**Start with per session (owner ruling, 2026-09-06).**

> "Start with per session."
> — Bob, 2026-09-06

§2.1's PM-operator story and §5.1's list route (and, symmetrically, §5.2's tree
route) are per session first: both routes require a `session=<id>`, reached
from a session's row in the Sessions console, and there is no bare unfiltered
route in this phase. The machine-wide aggregate view this spec's earlier draft
described as the default — an unfiltered, cross-session list or tree — is kept,
not deleted, and moved to "Deferred: machine-wide view" (§5.4): a later phase
reached through the Sessions console, composing per-session dashboards rather
than standing as its own top-level destination.

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

*I have dispatched several agents within one session, and I want to know which
agent is doing what right now.*

- **List.** One row per event in one `tm` session — `session=<id>`, opened from
  that session's row in the Sessions console — newest at the bottom. A row
  reads: timestamp, actor, kind, and a one-line object summary —
  "14:03:12 · rust-engineer · workflow.spawn · code-critic". A dispatch, a
  worktree creation, a hook firing, and a session ending are all rows in the
  same column.
- **Tree.** The session is the root. Each dispatched agent is a child node
  that appears when its `Workflow::Spawn` arrives and stays lit until its
  `Workflow::Stop`. Under an agent, its tool calls and file writes are leaves.
  The operator sees at a glance that four agents are live, one has been running
  for eleven minutes with no leaf activity, and one finished.
- **Click-through.** The agent node links to the session viewer; the file leaf
  links to the diff viewer; a `Workflow::Spawn` node links to the delegation's
  prompt and the worktree path.

*What this replaces:* reading one tmux pane, or `tm session status` on a loop.

*Running five sessions across three projects?* Each has its own dashboard,
opened from its own row in the Sessions console — one tab per session. A
single view showing every session's events unfiltered is not this phase's
deliverable; see "Deferred: machine-wide view" (§5.4).

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
  cadence. Machine-wide, unfiltered rotation is the "Deferred: machine-wide
  view" case (§5.4) — the wall display's own dashboard is a per-session one
  until that phase lands.
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

| Option | What it means | Status |
|---|---|---|
| **A. Console-hosted** | Console owns the bus; the harnesses push to it | **Decided.** Owner ruling, 2026-09-05 — see below |
| **B. A new crate** | `trusty-eventbus`, a fourth daemon | Rejected. ADR-0032 says no new service binds HTTP, and a UDS-only fourth daemon adds a supervision target, an install step, and a failure mode for no capability the existing crates lack. #3157 explicitly wanted *fewer* bus implementations |
| **C. `trusty-common::control_bus`** | The bus promoted into the common layer, as epic #3157 had scoped it | Superseded 2026-09-05. #3157 closed not-planned on 2026-09-02; the owner ruling below decides placement directly rather than through that epic |

**Decided: Option A, console-hosted (owner ruling, 2026-09-05).**

> "The only event bus actually."
> — Bob, 2026-09-05

`trusty-console` hosts the event bus, and it is the ONLY event bus in the
workspace. Producers — `trusty-mpm`, `trusty-code`, `trusty-agents`, and
`trusty-analyze`'s LSP events — push `HarnessEvent` frames to console over its
UDS ingest socket, through a shared bounded `PushClient` in `trusty-common`
(buffer default 4096, replay on reconnect, a `dropped` count on overflow; full
contract in §4.2). Console is the one process that assigns the single `seq`,
appends every accepted frame to a day-rotated NDJSON log, keeps an in-memory
ring (configurable, default 8192), and fans the stream out over SSE (§4.3,
§4.4).

`trusty-common` carries only what two or more processes need to agree on: the
envelope (`HarnessEvent` with `id`/`parent_id`, §3.1), the `ActionEvent`
taxonomy (§3.2), and `PushClient` itself. It holds no broadcast channel and no
process-global bus — that state lives in exactly one place, console, which is
the point of the ruling: ADR-0005 and epic #3157 both aimed the promotion at a
library every process loads and hosts a copy of; this ruling makes console the
single owner and everyone else a client of it.

`trusty-agents-common`'s in-process bus (`crates/trusty-agents-common/src/events/bus.rs`)
is retired once its producers push to console instead of publishing locally —
not before, so no consumer of the current API breaks mid-migration.

This supersedes Option C above and the 2026-07-18 "single hub on the tm
daemon" decision, which placed the hub role on `trusty-mpm` rather than
console. It is consistent with ADR-0032 (console is the single external
surface — this extends the same reasoning from HTTP to the event bus) and the
2026-08-30 push-to-console rulings that established the same producer-pushes
shape for other subsystems.

Option A's original objection — a harness running with console stopped loses
its events — is bounded, not eliminated, by `PushClient`'s local buffer: a
producer whose console connection is down keeps working and keeps buffering
(default 4096 frames), replaying on reconnect. Only a producer that stays
disconnected longer than its buffer holds loses events, and it reports how
many via the `dropped` count.

**Ownership boundary (owner ruling, 2026-09-05).**

> "The console crate should also hold dashboard code. It can call APIs in
> other crates."
> — Bob, 2026-09-05

`crates/trusty-console` owns all dashboard code — the UI views (§5), the
HTTP/SSE routes (§4.4), and the event-bus core this section decides (the
ingest socket, `seq` assignment, the ring, and the log, §4.2–§4.3). No other
crate gains dashboard code of its own. Console reaches every other crate only
through that crate's UDS or library API — the object viewer (§6) dials the
owning harness for a session record rather than reading its internals
directly, exactly as `service_metrics.rs` already dials `trusty-memory`,
`trusty-search`, and `trusty-analyze` today. Symmetrically, a producer crate
— `trusty-mpm`, `trusty-code`, `trusty-agents`, `trusty-analyze` — carries
only three things for this spec: the shared `trusty-common` event types
(§3.1–§3.2), the shared `PushClient` (§4.2), and its own publish call sites
(§3.4).

**Non-blocking invariant (owner ruling, 2026-09-05).**

> "[The bus] is a critical service though it shouldn't block functionality,
> just messages and observability."
> — Bob, 2026-09-05

The bus is worth building correctly, and its failure must never reach a
daemon's actual work. Bus unavailability — console down, the ingest socket
unreachable, a full `PushClient` buffer — degrades message delivery and
observability only: events queue, then drop with a counted gap (§4.2), and
the dashboard shows a disconnected state (§8.5). It never delays a tool call,
blocks a dispatch, or fails a session. §4.3's backpressure rule already
states the mechanism; this is the invariant the mechanism exists to serve.

### 4.2 Ingestion — the push contract

Producers push; console is the only ingester. There is no per-daemon cursor
method for console to drain — §4.1's ruling retires the pull-based
`bus_events { since_seq, max }` shape this section specified before
2026-09-05.

- **In-process, inside each harness.** A harness's adapter (§3.4) builds a
  `HarnessEvent` exactly as before and hands it to `PushClient` instead of
  broadcasting locally. `PushClient` stamps nothing — `id` is minted by the
  emitting process (§3.1); `seq` is stamped once, by console, on arrival
  (§4.3).
- **Child-to-parent stderr relay.** Unchanged. A subprocess writes one NDJSON
  line prefixed `__OMPM_EVENT__ ` and the parent re-publishes it. This already
  works (`EVENT_LINE_PREFIX`, `bus.rs:53`) and is how a `--workflow` child
  reaches its API server today. The parent's own adapter then pushes the
  re-published event to console exactly as it would one of its own.
- **`PushClient`, over console's UDS ingest socket.** One shared client in
  `trusty-common`, used by every producer:

  ```
  PushClient::send(event)
    -> enqueue in a local bounded buffer, flush to console's ingest socket
  ```

  - **Frame shape.** One `HarnessEvent` per frame, newline-delimited JSON — the
    push contract changes the transport, not the envelope.
  - **The ingest socket.** Console binds one UDS socket for this, using the
    same per-daemon convention every other socket in the workspace already
    uses (`trusty_common::daemon_socket_path`,
    [port-assignments.md](../architecture/port-assignments.md)):
    `daemon_socket_path("trusty-console")` resolves to
    `<data dir>/trusty-console/trusty-console.sock`. Every producer, regardless
    of source, dials this one socket.
  - **Buffering.** `PushClient` holds a local bounded buffer, default capacity
    4096 frames, so a producer never blocks on a slow or absent console —
    `send` is best-effort from the producer's point of view, the same rule
    §4.3 states for backpressure.
  - **Reconnect and replay.** On a dropped connection, `PushClient` reconnects
    with the same backoff shape used elsewhere in the workspace (§8.1) and
    replays whatever is still buffered, oldest first, before resuming live
    sends — a console restart or a brief network hiccup loses nothing that
    still fits in the buffer.
  - **Overflow: the `dropped` count.** When the buffer fills before a
    reconnect succeeds, `PushClient` drops the oldest frames and keeps a
    running `dropped` count, surfaced the same way `Lag { skipped }` already
    is (§4.3) — a gap is always visible to the viewer, never silent.

  ADR-0032 holds throughout: no harness binds HTTP, console is the only HTTP
  surface, and this UDS socket is console's ingest side of the same aggregation
  ADR-0035 already directs for reads.

### 4.3 Ordering, retention, and backpressure — all console-side

**Ordering.** `seq` is now minted once, by console, for every frame it accepts
— not per-process. §4.1's ruling removes the cross-process ordering problem
this section used to solve: with exactly one process assigning `seq`, `seq`
alone is already a total, deterministic order, and the list view uses it
directly. `parent_id` still carries the causal edge the tree view needs (§5.2)
— `seq` order and causal order answer different questions, and the tree keeps
using the second one.

Clock skew no longer affects ordering (`at` is still recorded and still used
for display and duration arithmetic). A cross-machine bus is still out of
scope, and this spec still has no answer for one (§10 Q3).

**Retention.** Two tiers, both held by console.

- **The ring.** An in-memory ring, capacity configurable, defaulting to 8192,
  held by console rather than by each harness. Live subscribers — the SSE
  route (§4.4) — read this. Overflow is reported, never silent: a ring overrun
  produces the same `Lag { skipped }`-shaped notice §3.1's envelope already
  carries, and the dashboard renders it as a visible gap marker in both views.
- **The log.** A durable NDJSON file per day, written by console as each frame
  is accepted, rotated and retained per #3157's original "durable JSONL
  logging, rotation, retention, and replay" requirement — the requirement
  survives the epic's closure even though its bus placement did not. A viewer
  that opens mid-session reads the log to build the tree it missed, then
  switches to the live ring at the log's last `seq`. Without this, the tree
  view is empty until the next spawn and a wall display shows nothing after a
  console restart.

**Backpressure.** The push contract (§4.2) is what keeps a producer from ever
blocking on the bus, not a best-effort broadcast inside a shared library. A
producer whose `PushClient` cannot reach console keeps working and buffers
locally (default 4096 frames) rather than stalling; a slow SSE subscriber gets
a `Lag` notice from console's own ring and never slows a producer down,
because producers push and never wait on a reader. The rule is explicit
because the alternative — a full channel stalling an agent's tool call — would
make telemetry able to break the work it observes.

**Overflow, three defenses.**

1. The console ring's capacity is configurable, defaulting to 8192 — sized for
   three (soon four) harnesses' actions rather than one harness's lifecycle.
2. Producers push independently of any reader, so a browser that stops reading
   lags only console's own SSE buffer (§8.2) — never the ring, and never a
   producer.
3. The log is written by console as frames arrive, so a lagged SSE reader
   loses nothing permanently — it re-reads from the log via `since_seq`.

### 4.4 Fan-out to viewers

Console is the only HTTP surface and, per §4.1, the only event bus — the
frames every producer pushed to console's ingest socket (§4.2) and the ring
and log console built from them (§4.3) are what these routes read. Every
viewer reads console; nothing upstream of console is a fan-out point of its
own.

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

**DOC-72's analyze events move onto the push contract, not around it.** DOC-72
§4 already publishes LSP results as `HarnessEvent`-shaped payloads into a
per-`(workspace, language)` ring, today drained by `analyze.lsp_events {
since_seq, max } -> {events, next_seq, dropped}` from `metrics_poller.rs` and
republished at `/api/console/events/analyze/lsp`. Under the 2026-09-05 ruling
that per-daemon cursor is retired the same way §4.2 retires every other
service's: `trusty-analyze` becomes a `PushClient` producer alongside
`trusty-mpm`, `trusty-code`, and `trusty-agents`, pushing over console's one
ingest socket instead of being polled. Two reconciliations:

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

Sequential, newest last, scrolling, per session (owner ruling, 2026-09-06 —
"Start with per session."; §1).

- **`session=<id>` is required.** The route always scopes to one session and
  never renders unfiltered by default. It is reached from that session's row in
  the Sessions console (§1) — `/ui/stream/list?session=<id>` — not as a
  standalone destination a user navigates to directly. A request with no
  `session` is the "Deferred: machine-wide view" case (§5.4), not something
  this phase serves.
- **Virtualized.** Only the visible rows exist in the DOM. A day-long session
  produces tens of thousands of events and an unvirtualized list stops scrolling
  smoothly in the low thousands.
- **A bounded window.** The client holds at most `N` events in memory (default
  20 000, §8.2) and drops from the head. Older events are reachable through the
  cursor route, not by scrolling forever.
- **Follow-tail by default.** The view pins to the newest row. A wall display
  never needs to be scrolled to stay current.
- **Further filters live in the URL**, additive to the required `session`:
  `?session=<id>&source=mpm&kind=file&actor=rust-engineer`. Combining axes ANDs
  them, matching `events::Filter`'s existing semantics.
- **A row is:** timestamp (mono), source badge, session, actor, kind badge, and
  the first `ObjectRef`'s label as a link. Foundry's typographic split applies —
  IBM Plex Mono for every identifier, path, count, and status, IBM Plex Sans for
  the label (DOC-39 §8).
- **Gap markers.** A `Lag { skipped }` renders as a full-width mono rule reading
  the skipped count. It is never omitted.

### 5.2 Tree view — `/ui/stream/tree`

A call graph, keyed on `parent_id`. Like the list view, `session=<id>` is
required (§5.1) — the tree route is also per session first, reached from the
same Sessions-console row.

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

### 5.4 Deferred: machine-wide view

**Not built in this phase.** §5.1 and §5.2 require `session=<id>` on every
list and tree route (owner ruling, 2026-09-06 — "Start with per session.";
§1). A machine-wide view — every live session's events in one unfiltered list
or tree, as the pre-2026-09-06 draft of this spec described as the default —
is kept, not deleted. It is deferred to a later phase, reached through the
Sessions console once that phase lands, and composes the per-session
dashboards rather than replacing them.

- **What it would add.** A route with no required `session` (or an explicit
  `session=all`) showing every live session's events interleaved. §5.1's and
  §5.2's per-session mechanics — virtualization, the bounded window,
  follow-tail, node lifecycle, timed collapse — carry over unchanged; only the
  query's scope widens.
- **Who it serves, once built.** The PM operator running several sessions at
  once (§2.1) and the wall display's unfiltered rotation (§2.4) both describe
  this view. Until it lands, each gets a per-session dashboard per session
  instead.
- **Milestone 72 predates this ruling.** Its title,
  "trusty-mpm dashboard — machine-wide event visualization (DOC-73)", names
  what is now the deferred phase this subsection describes, not the
  per-session phase §9 orders first. Re-titling or splitting milestone 72 is a
  milestone-tracker follow-up, not a change this spec makes.

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

**Q1 — Where does the bus live?** **Answered, 2026-09-05.** Owner ruling: "The
only event bus actually." `trusty-console` hosts the event bus and is the ONLY
event bus; producers push over UDS through a shared `trusty-common::PushClient`
(§4.1). `trusty-common` carries only the envelope, the `ActionEvent` taxonomy,
and `PushClient` — no broadcast channel, no `control_bus` module, no
process-global. This also settles the choice between a console-hosted bus and
a fourth daemon that §4.1's original recommendation left open on rejection: it
is console, not a new daemon.

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

Milestone 72 cut these as ten slices, re-scoped to the 2026-09-05
push-to-console ruling (§4.1). Each references #6611. Milestone 72's title
now names the deferred machine-wide phase, not the per-session phase these
slices build first — see §5.4.

1. [#6846](https://github.com/bobmatnyc/trusty-tools/issues/6846) — Types-only
   move: relocate the `HarnessEvent` envelope and lifecycle/filter types from
   `trusty-agents-common` into `trusty-common`, no behavior change, so every
   later slice lands on stable types.
2. [#6847](https://github.com/bobmatnyc/trusty-tools/issues/6847) — Envelope
   fields (`id`, `parent_id`) plus the `ActionEvent` payload arm plus
   `PushClient`, in `trusty-common` (§3.1, §3.2, §4.2).
3. [#6848](https://github.com/bobmatnyc/trusty-tools/issues/6848) — The
   console event-bus core: the UDS ingest socket, single-`seq` assignment, and
   the durable day-rotated NDJSON log (§4.1, §4.3).
4. [#6849](https://github.com/bobmatnyc/trusty-tools/issues/6849) —
   `trusty-mpm` emits `ActionEvent` with `parent_id` threading and the named-hook
   projection, pushed via `PushClient` (§3.4, §4.2).
5. [#6850](https://github.com/bobmatnyc/trusty-tools/issues/6850) — Console's
   list route over the ring: `GET /api/console/events/stream` with
   `since_seq`/`source`/`session`/`kind`/`actor` filters (§4.4).
6. [#6851](https://github.com/bobmatnyc/trusty-tools/issues/6851) — SSE
   fan-out: `GET /api/console/events/stream/sse` with `Last-Event-ID` resume
   (§4.4, §8.1).
7. [#6852](https://github.com/bobmatnyc/trusty-tools/issues/6852) — The list
   UI: `/ui/stream/list`, virtualized, URL filters, follow-tail, bounded
   window, gap markers (§5.1).
8. [#6853](https://github.com/bobmatnyc/trusty-tools/issues/6853) — The tree
   UI: `/ui/stream/tree`, `d3-hierarchy` layout, node lifecycle, stall marker,
   timed collapse, forest layout, with the 2000-node/300ms measurement (§5.2,
   §7.3).
9. [#6854](https://github.com/bobmatnyc/trusty-tools/issues/6854) —
   `trusty-code` and `trusty-agents` publish `ActionEvent` via `PushClient`;
   `trusty-agents-common`'s in-process bus is retired now that its producers
   push to console instead (§3.4, §4.1).
10. [#6855](https://github.com/bobmatnyc/trusty-tools/issues/6855) — The
    object viewer: `/ui/object/<type>/<id>` for every type in §6, read-only,
    new-tab, stated-absence handling, plus list/tree in the screensaver
    rotation (§5.3, §6).
---

## {#SPEC-UNIDASH-12~draft} 12. Design unification

> "unify the console/dashboard designs, based on the new DS and console. All
> dashboards should link up to console, and only show detailed information."
> — Bob, 2026-09-06

Three rules follow, and they generalize §1's placement rule beyond the
mpm/code/agents dashboard this document originally scoped, to every trusty-*
dashboard.

1. **One design language.** Every trusty-* dashboard uses Foundry
   (`docs/design/UI/design-system/`). Console's own copy
   (`crates/trusty-console/ui/src/foundry.css`, `theme.css`) is a separate
   file tree, not a symlink to the canonical one, so the two can drift; console
   stays the reference deployment regardless. A dashboard that carries no
   Foundry tokens, or one not yet verified against them, is not unified. Four
   surfaces are unverified today: `trusty-mpm-gui`
   (`crates/trusty-mpm-gui/ui/src/`), the `tagent` desktop UI
   (`crates/trusty-agents/ui/src/`), `trusty-code-gui`
   (`crates/trusty-code-gui/ui/src/`), and `trusty-audit`'s UI
   (`crates/trusty-audit/ui/src/App.svelte`) — none of them was found to carry
   a `tokens.css`.
2. **Every dashboard is reachable from console.** Console is the entry point
   for every trusty-* dashboard, following #6155's precedent: `trusty-search`'s
   dashboard already migrated in and is served by the console binary at
   `/tools/search` (`crates/trusty-console/ui-search/`,
   `crates/trusty-search/build.rs:9` records that the crate itself carries no
   `ui/` tree any more). Four dashboards are not reachable from console yet,
   each living behind its own process, daemon, or window instead: `trusty-memory`'s
   admin UI (its own HTTP daemon binary), `trusty-analyze`'s UI (its own
   on-demand process, invoked and read over stdio MCP, never proxied),
   `trusty-mpm-gui` and the `tagent` UI (separate Tauri desktop windows), and
   `trusty-code-gui` (its own process). §13 states what each shows once
   reachable, and §17's Sequence orders the migrations.
3. **A dashboard shows detail only.** Console's own tabs display state; a
   dashboard is where the detail behind a console row lives (§14), and where
   any control lives (§13). A dashboard never re-summarizes what console
   already shows — it is reached FROM a summary, and it goes deeper, not
   sideways.

## {#SPEC-UNIDASH-13~draft} 13. Console displays, dashboard manages

> "console displays, dashboard manages. All interactive/management/
> configuration elements move to the dashboard view."
> — Bob, 2026-09-06

**The division.** A console screen shows state: it reads, it renders, it
navigates. It never mutates anything. A dashboard is where a person changes
something — creates, deletes, configures, compacts, or executes an action.
Every control that writes moves to the dashboard for the element it acts on.

**Migration backlog.** Five places in console carried a MANAGE control when this
section was written, verified against `crates/trusty-console/ui/src/*.svelte`.
The first two are DONE
([#6923](https://github.com/bobmatnyc/trusty-tools/issues/6923)); the line
numbers below are the ones they carried then.

1. ~~**`SearchTab.svelte`** deletes an index inline, through
   `DeleteAction.svelte:74,96,113` (`SearchTab.svelte:194`, a `DELETE` fetch).~~
   Removed; the row links to `/tools/search/#/indexes/<id>/config` instead (§14).
2. ~~**The `StaleIndexCleanup` panel nested inside `SearchTab.svelte`** scans and
   prunes stale index registrations
   (`StaleIndexCleanup.svelte:151,203,240,288,289,296,299`,
   `POST /api/console/search/prune-indexes`, invoked from
   `SearchTab.svelte:206`), and, nested a level further inside it, deregisters
   unjudged registrations (`UnjudgedReview.svelte:73,109,110,116,117`,
   `POST /api/console/search/deregister-unjudged`, embedded at
   `StaleIndexCleanup.svelte:313`).~~ Both components are deleted. Their routes
   still serve; no console screen calls them until the search dashboard carries
   the panel, which is not yet cut as an issue.
3. **`MemoryTab.svelte`** compacts a palace inline, through
   `CompactAction.svelte:48,70,83` (`MemoryTab.svelte:212`, `POST`), and
   deletes one, through `DeleteAction.svelte:74,96,113`
   (`MemoryTab.svelte:213`, `DELETE`).
4. **`ConfigTab.svelte`** saves the mpm config
   (`ConfigTab.svelte:106,140`, fetch at `:73`,
   `POST /api/console/config/mpm`) and toggles auto-resume
   (`ConfigTab.svelte:124`) as part of the same submit.
5. **`SessionsTab.svelte`** spawns a session
   (`SessionsTab.svelte:295,302-305`, `POST`), toggles the supervisor's
   auto-resume (`SessionsTab.svelte:335,337`,
   `POST /api/console/sessions/supervisor/auto-resume`), bulk-deletes session
   records (`SessionsTab.svelte:371-402`,
   `POST /api/console/sessions/bulk-delete`), and resumes, stops, or
   decommissions a session per row (`SessionsTab.svelte:437,440,443`). Its
   target dashboard is settled (owner ruling, 2026-09-06 — "Port it into the
   console"): `trusty-mpm-gui`'s `Dashboard.svelte` and `SessionDetail.svelte`
   are ported into a console-hosted `/tools/mpm` page, the same shape as
   `/tools/search` and the #6155 migrations. The Tauri app is not launched
   from console — it is replaced by the ported page, not wrapped by it. Once
   the port lands, `SessionsTab.svelte` becomes DISPLAY-only and each row
   links to `/tools/mpm/sessions/<id>` (§14).

**The session row also gains an agent roll-up (owner ruling, 2026-09-06).**
Each session row — in console's Sessions view and in the ported `/tools/mpm`
page — expands to its live subagents, showing a state glyph, agent type,
current one-line activity, elapsed time, and tokens consumed, matching the
Claude Code status line: `◯ rust-engineer  Appending probe test to
ops_tests.rs  1h 7m 15s · ↓ 281.5k tokens`. A distinct waiting-for-user-input
signal — a Foundry status colour plus icon, not the same treatment as a
routine idle state — marks any session or agent stopped on a question or a
permission prompt. That signal is sourced from the activity monitor's
existing classification: `ActivityState::BlockedOnPermission`
(`crates/trusty-mpm/src/activity/cache.rs:25`) is the state already computed
for exactly this condition, and `ActivityVerdict.summary` on the same struct
is the one-line activity text the roll-up renders. The roll-up is
DISPLAY-only in console; answering a prompt or resuming a session is a
dashboard action, on the ported `/tools/mpm` page. Data reaches the row over
#6460's push transport, the same path §15's pulses use.

**Already DISPLAY-only, no change needed.** `AnalyzeTab.svelte`'s one
interactive element is a `<select>` at `:287` that drives a GET-only fetch
(`fetchIndexes`/`loadViz`, `:98-155`); `ReviewTab.svelte` issues one GET
(`:32`) and has no buttons; `ServicesList.svelte`'s row click (`:88-92`)
switches console's own `activeTab` state and makes no write call;
`MachineStatusPanel.svelte` has no interactive elements at all.

Moving each of the five controls above to its target dashboard is a separate
step, ordered in §17's Sequence — this section states the rule and the
backlog, not the implementation.

## {#SPEC-UNIDASH-14~draft} 14. Row-click contract

> "Where the console has an element list (sessions, palaces, indexes),
> clicking goes to the management view for that element."
> — Bob, 2026-09-06

Every row in a console element list links to that element's management view.
The table states, per list: where it renders today, what a row click does
today, what it should open once §13 lands, and whether that target exists yet.

| List | Rendered in | Row click today | Target management view | Exists today? |
|---|---|---|---|---|
| Services | `ServicesList.svelte`, Overview tab | `onOpen` switches `activeTab` to that service's in-console tab (`App.svelte:50-59,121-125`) — never leaves the console shell; since #6923 a service whose OWN daemon serves the dashboard is an `<a>` to the URL detection read from its `http_addr` file | The service's own dashboard: search → `/tools/search`; agents → its daemon's SPA root (#6923); memory/analyze/mpm → none yet | Search and agents have real targets |
| Indexes | `SearchTab.svelte` grid list | DONE (#6923) — the row IS the link, to `/tools/search/#/indexes/<id>/config`. The dashboard is hash-routed, so there is no `/tools/search/indexes/<id>` path to link to | `crates/trusty-console/ui-search`'s `IndexConfig.svelte`, reached from `Indexes.svelte` (`#/indexes`, `#/indexes/{id}/config`) | Yes, already built and richer (create, bulk reindex/delete, per-row config) than the console tab's inline delete was |
| Palaces | `MemoryTab.svelte` table | No navigation — inline `CompactAction`/`DeleteAction` (§13, item 3) | No console-hosted memory dashboard exists yet. `crates/trusty-memory/ui`'s `Palaces.svelte` is read-only today (counts, a graph link) | Partial — the standalone view exists but has no create/delete/compact controls |
| Sessions | `SessionsTab.svelte` cards | No row-click navigation — every row carries inline Resume/Stop/Decommission (§13, item 5) plus an Activity toggle | A new console-hosted `/tools/mpm` page, ported from `trusty-mpm-gui`'s `Dashboard.svelte`/`SessionDetail.svelte` (owner ruling, 2026-09-06 — "Port it into the console"; §13, item 5) | Not yet — the port is [#6924](https://github.com/bobmatnyc/trusty-tools/issues/6924), sequenced after #6460 |
| Agents | none | n/a | n/a (the SERVICE row links to the daemon's own dashboard since #6923; a per-agent list is still unbuilt) | No agents element list exists in console yet. `crates/trusty-console/src/detect/agents.rs` detects `trusty-agents` for the Overview row only; the per-agent list lives only in the standalone `tagent` desktop app |
| Reviews | none (aggregate only) | n/a | n/a | `ReviewTab.svelte` shows one aggregate report; there is no per-PR row to click |

Agents and Reviews carry no element list today, so the row-click contract has
nothing to bind yet for either. An eventual Agents list is this document's own
phased delivery (§9) to design, not a new item cut here.

## {#SPEC-UNIDASH-15~draft} 15. Activity pulses

> "use color pulses in console to indicate actions: read/write/index"
> — Bob, 2026-09-06

### 15.1 Extending the taxonomy

§3.2's `ActionEvent` has no kind for an index operation, and `File`'s existing
phases (`Read`, `Written`, `Created`, `Deleted`, `Moved`) do not cover one
either — §3.4 already states plainly that "nothing about `File` fits today."
This ruling adds a sixth kind, alongside `Workflow`, `Agent`, `File`, `Tool`,
`Session`, and `Inference`:

```rust
Index { phase: CallPhase, actor: Actor, target: ObjectRef, .. }
```

using the same `CallPhase` (`Started`, `Finished`, `Errored`) that `Tool` and
`Inference` already use. Per §3.3, this is an additive new `kind` and does not
bump `schema_version`.

The three colours the ruling asks for map onto three kinds, one of them new:

- **Read** — a `File::Read`, or a read-phase `Workflow`/`Tool` event.
- **Write** — a `File::Written`/`Created`/`Deleted`/`Moved`, or
  `Workflow::Write`.
- **Index** — the new `Index` kind above.

### 15.2 Producers

Three producers push these events over console's ingest socket (§4.2),
through the shared `PushClient`:

- `trusty-mpm` — already a producer (§3.4); its session and file events
  already carry `Read`/`Write`.
- `trusty-search` — a new producer, not in §4.1's producer list today. Its
  `index_file` and `reindex` calls become `Index` events.
- `trusty-memory` — a new producer, not in §4.1's producer list today. It
  exposes only a per-palace `last_used_unix` timestamp over `console_metrics`
  today (`crates/trusty-memory/src/console_metrics/mod.rs:1-60`), a
  poll-driven gauge with no read-versus-write distinction and no per-event
  granularity; becoming a `PushClient` producer is new work, not a reuse of
  that field.

All three depend on #6460 landing — the services-push-metrics-to-console
transport epic (epic #6284). #6460 is in progress, not merged, as of this
amendment; a pulse producer has nothing to push over until it does.

### 15.3 Render points

A pulse attaches to an element already on screen, never a new widget:

- Services rows, beside the existing status `Badge`
  (`ServicesList.svelte`, `.badge-cell`).
- `SessionsTab.svelte`'s session-card header
  (`.session-name`/`.session-state`).
- `MemoryTab.svelte`'s palace rows, beside the existing count badges.
- `SearchTab.svelte`'s index rows, beside the id/`Last Used` cells. Since
  #6923 the roster is a grid list whose row is a link, so the pulse attaches to
  a cell inside that link, not to a `<td>`.

### 15.4 Foundry addition

No pulse motion token exists today. `foundry.css:240-288` defines the robot
mark's `robot-idle`/`robot-working`/`robot-receiving` blink/tilt/ping states,
but those animate one mascot icon and carry no colour-by-kind meaning — all
three states use the same body colour, only motion differs. Foundry gains:

- **A new `@keyframes` pulse** — a short scale/opacity flash, distinct from
  the robot mark's blink/tilt/ping, applicable to an arbitrary row or badge.
- **A colour per action class.** `--trusty-info` for read and
  `--trusty-accent` for write reuse Foundry's existing roles (`tokens.css:32-44`
  light, `:105-107` dark). Index needs one new token — `--trusty-warning`
  already means "degraded" (§8.4's role mapping), so reusing it for "index" would
  read as a fault when none exists.

## {#SPEC-UNIDASH-16~draft} 16. The Disk dashboard

> "Design the Disk dashboard too, that should show projects and worktrees,
> segmented radial style like daisy disk, with indications for stale
> worktrees and an ability to clear them."
> — Bob, 2026-09-06

This section amends DOC-73 with a new console surface: **projects and
worktrees, rendered as a segmented radial (sunburst) view, with staleness
shown by colour and clearing gated behind an explicit confirm.** It follows
this document's own placement rule (§1, §13): the Disk *view* is a console
display; the Disk *dashboard* — the clear action — is where trusty-mpm is
managed, reached from that view.

### 16.1 Data sources

- **Project registry.** `crates/trusty-mpm/src/project/registry.rs:34-90` —
  `ProjectRegistry` wraps a `ProjectStore` (`<data_dir>/projects.json`),
  exposed over MCP as `project_list`/`project_get`. `Project` carries no size
  field today.
- **Worktree discovery.** `crates/trusty-mpm/src/session_manager/worktree_registry.rs:1-45`
  derives worktrees from `git worktree list --porcelain` and
  `git rev-parse --git-common-dir` (#4207), not from path-shape guessing. A
  managed project can have two git checkouts, each owning its own worktree
  registry — the project checkout itself, and the `.base` bare clone
  trusty-mpm provisions alongside it (`worktree_registry.rs:58-60`). Both must
  be walked to see every worktree for a project.
- **Path shapes.** `.worktrees/<name>` (project-owned session worktrees) and
  `.claude/worktrees/<name>` (the Claude Code harness's own agent-lifetime
  store, `worktree_ownership.rs:261-267`). One predicate,
  `decommission::removal_permitted`, now admits both shapes plus a
  `.trusty-mpm-worktree` sentinel (post-#6561), shared by the classifier and
  the remover.
- **Byte measurement.** Two existing primitives, neither cached:
  `measure_bytes_until` (`worktree_reclaim.rs:1019-1044`, `walkdir`-based,
  deadline-bounded, used only inside the merged-PR survey) and
  `dir_size_bytes` (`crates/trusty-common/src/sys_metrics.rs:517-525`,
  iterative, depth- and time-capped, returns a partial total on truncation,
  runs today on a 60-second ticker for `/health`'s `disk_bytes`). Neither is
  an index: both re-stat every file on every call. A sunburst spanning many
  projects and worktrees at DaisyDisk's scale (hundreds of GB to TiB) cannot
  walk cold on every load — §16.6's issue list adds a cached size index as a
  prerequisite.
- **Live-session and git state.** `session_list`/`session_status` over MCP
  give live-session detection; `classify`'s own gates 5 and 6
  (`worktree_reclaim.rs:670-745`) already compute the merged/open/unmerged/
  no-PR state and the dirty/clean state the sunburst needs to display — the
  display reuses the survey's output, adding no new git probe.

### 16.2 Staleness — three tiers mapped to `classify()`'s verdicts

The tier a worktree shows is a projection of the same `ReclaimVerdict`
`tm session prune-worktrees` already computes, not a second predicate the
display and the clear action could disagree about.

| Tier | Predicate | Computed by | Cost |
|---|---|---|---|
| **SAFE-TO-CLEAR** | Gates 1-4 pass (admitted, not live-claimed, tm-provisioned, no live agent) AND gate 5 is `Merged{pr}` AND gate 6 is clean AND the path is not on the owner keep-list | `classify()` returning `Reclaimable`, plus a keep-list check the classifier does not have today | One `classify()` call per worktree: a few git subprocess calls, one `gh pr view`, one dirty-tree probe |
| **REVIEW** | Clean tree, but PR state is `Open`, `ClosedUnmerged`, `NoPr`, `Unknown`, or `LookupFailed` | The same call, returning a non-`Reclaimable`, non-agent, non-liveness `Blocked{gate: PrState}` with clean dirt | Same as above |
| **KEEP** | Dirty tree, unpushed commits, a live session claim, a live dispatched agent, or a keep-list entry | `Blocked{gate: Liveness}` / `BlockedByAgent` / `Blocked{gate: UnsavedWork}`, or a keep-list membership check | Same as above; the keep-list check is an O(1) path lookup |

A branchless worktree resolves to `NoPr` at gate 5 — REVIEW, not
automatically SAFE, absent a keep-list override. Every tier is
first-gate-wins, in `classify`'s own gate order: a dirty, unmerged worktree
reports KEEP, because dirt outranks PR state for display purposes exactly as
it does for the CLI's own verdict.

**The keep-list gate is new.** Nothing in `worktree_reclaim.rs` today accepts
an owner-authored allowlist. It is added as a seventh gate (or a new
`Admission` variant), so a keep-listed worktree renders `Blocked`, never
silently invisible.

### 16.3 Visual design

**Hand-rolled SVG, not a charting library.** `trusty-console/ui`'s own
`BarGraph.svelte` (`crates/trusty-console/ui/src/BarGraph.svelte:1-24`) is
deliberately inline-SVG-only, with no charting library and no canvas, so
console's existing charts cannot drift against each other. `d3` is a
dependency of `trusty-analyze/ui` only (`crates/trusty-analyze/ui/package.json:14`),
a separate Svelte bundle — pulling it into `trusty-console/ui` for one
sunburst breaks that convention for a single view. The sunburst follows
`BarGraph.svelte`'s pattern: path-string generation in a co-located `.js`
module (`diskSunburst.js`), tested independently of the component
(`diskSunburst.test.js`).

**Rings.** Centre is the workspace-root total; ring 1 is one arc per
registered project (angle proportional to bytes); ring 2 is one arc per
worktree within that project, across both path shapes and the `.base` bare
clone; an optional ring 3 is the top-N subtrees inside a worktree (`target/`,
`node_modules/`, `.git/`), with a synthetic "other" wedge for the remainder so
ring 3 never needs full enumeration.

**Colour, mapped to the three tiers using existing tokens — no new colour
roles:**

- SAFE-TO-CLEAR → `--trusty-success` / `--trusty-success-soft`
  (`tokens.css:37-38` light, `:110-112` dark).
- REVIEW → `--trusty-warning` / `--trusty-warning-soft`
  (`tokens.css:39-40`, `:112-113`).
- KEEP → `--trusty-danger` / `--trusty-danger-soft`
  (`tokens.css:41-42`, `:113-115`).
- Ring 1 (projects) and ring 3 (subtrees) carry no staleness colour — a
  neutral surface/border tone, so colour means one thing only.

**Interaction.** Hovering or clicking a ring-2 arc opens a detail panel:
path, bytes, branch, tier plus the `ReclaimGate` label and reason string
verbatim, last commit date, PR number and state, and the owning session or
agent if any — a direct render of the `ReclaimCandidate`/`ReclaimVerdict`
struct (`worktree_reclaim.rs:780+`), not a new computation. A legend (Safe to
clear / Review / Keep / Project) stays visible at all times, never
hover-only. Any ring-2 or ring-3 arc under a 2° minimum angle collapses into a
grey "other (N items, X GB)" wedge, expandable to a flat list.

**Narrow-width fallback.** A sunburst degrades badly under roughly 600px —
labels become unreadable and touch targets too small. Below that width, the
view falls back to a flat, sortable list — project, then worktree rows,
tier shown as a `Badge.svelte` — sorted by bytes descending. The clear
confirmation flow (§16.4) needs a list regardless of width, so the fallback
reuses that same component.

**ASCII layout sketch, desktop width:**

```
+-----------------------------------------------------------------+
| Disk                              [Legend: o Safe o Review o Keep]|
|                                                                   |
|                    .-----------.                                 |
|                 .--|  ring 2   |--.      +- Detail panel -------+ |
|              .--|  | worktrees |  |--.   | path: .../pr-6907     | |
|           .--|  .--|  ring 1   |--.  |--.| branch: fix/uds-buf   | |
|          +-+ |     |  projects |     | +-+ tier: SAFE-TO-CLEAR   | |
|          | | |     '-----+-----'     | | | reason: merged #6907  | |
|          +-+ '--.  (center: total) .--' +-+ bytes: 1.2 GB        | |
|           '--.  '--------+--------'  .--'   last commit: 2d ago | |
|              '-----------+-----------'      PR: #6907 merged    | |
|                                              session: none       | |
|                                              [Open in dashboard] | |
+-----------------------------------------------------------------+
```

### 16.4 Management actions, and the guards on clearing

**The pattern to follow already exists.** `crates/trusty-console/src/routes/cleanup.rs`
implements this exact shape for stale search-index registrations: a census, an
operator review, an explicit confirm, then a batch action reporting a per-item
outcome, never a single boolean "cleaned." Its UI half,
`crates/trusty-console/ui/src/StaleIndexCleanup.svelte`, was removed from the
console by §13's display-only ruling
([#6923](https://github.com/bobmatnyc/trusty-tools/issues/6923)) and now lives
only in history — read it with `git show
53e067931:crates/trusty-console/ui/src/StaleIndexCleanup.svelte`. The Disk
dashboard follows it:

- **Clear one worktree.** The confirm dialog names the exact path, branch,
  PR, and bytes to be reclaimed, from the `ReclaimCandidate` already rendered
  in the detail panel — no re-fetch needed.
- **Clear all SAFE-TO-CLEAR.** The confirm dialog lists every candidate
  (path plus bytes) and a running total ("12 worktrees, 34.2 GB").
- **Dry-run preview.** `tm session prune-worktrees` already defaults to
  dry-run, and the HTTP route already takes a `dry_run` field
  (`crates/trusty-mpm/src/daemon/managed_routes/prune.rs:128-155`). The
  dashboard's review stage *is* this existing dry-run call — no new concept,
  just surfacing it.

**The MCP tool gap.** Console reaches trusty-mpm only through its MCP tool
interface (`crates/trusty-console/src/routes/config.rs:1-13` states the
discipline: HTTP lives only in console, driven natively through trusty-mpm's
MCP tools, never by proxying to the daemon's own HTTP port). trusty-mpm's
`prune-worktrees` has three call sites today
(`crates/trusty-mpm/src/daemon/managed_routes/prune.rs:63-70`): the orphan-GC
loop, an MCP tool that is orphan-only (no merged-PR argument), and the HTTP
route (which does take `merged_prs`, but console cannot call a sibling
daemon's HTTP port). **No existing MCP tool exposes the merged-PR survey or a
reclaim-by-id call.** One is new work, mirroring `session_prune`'s shape
(`crates/trusty-mpm/src/mcp/mod.rs:339`, dispatched in
`crates/trusty-mpm/src/mcp/session_dispatch.rs:102`).

**Guards on the clear action.**

- **Removal executes only from the dashboard's explicit confirm, with a
  dry-run preview shown first.** There is no automatic or scheduled clearing
  from this surface.
- **The server re-checks every gate immediately before deleting**, mirroring
  `recheck_before_delete` (`worktree_reclaim.rs:303-345`), which already
  re-probes right before removal to close the survey-to-delete race. The
  dashboard never trusts an earlier snapshot for the delete itself.
- **Never a live session's worktree** (gate 2, `ClaimState`) **and never a
  live dispatched agent's worktree** (gate 4) — the clear action must not
  touch a KEEP-tier item, full stop.
- **Never the main checkout.** Gate 1's `Admission::Admitted` already
  excludes it structurally; this dashboard keeps that exclusion rather than
  re-deriving it.
- **The owner keep-list overrides every other gate to KEEP.**
- **This dashboard's confirm click is the human action BASE-AGENT's
  worktree-removal rule requires (#5791).** That rule bars a subagent from
  running `git worktree remove` on its own judgment; a person clicking
  confirm in a console the owner is running is that owner's own hand, not an
  autonomous agent decision, and satisfies the rule rather than working
  around it.
- **Audit logging is new work.** Today's only record is
  `tracing::info!`/`warn!` (e.g. `prune.rs:1169`,
  "prune-worktrees: removing orphaned worktree"). A dashboard-triggered clear
  logs the same way, plus records which console session or operator
  confirmed it — that attribution does not exist today.

### 16.5 API shape

```
GET  /api/console/disk/tree
  -> { generated_at, root: { path, bytes, projects: [
        { name, path, bytes, worktrees: [
            { id, path, branch, tier: "safe"|"review"|"keep",
              gate: "admission"|"liveness"|"removability"|"agent_ownership"
                    |"pr_state"|"unsaved_work"|null,
              reason, bytes, pr: {number, state} | null,
              last_commit_at, session: {id, state} | null,
              agent: {id} | null }
        ] }
      ] } }

GET  /api/console/disk/worktrees/{id}
  -> single ReclaimCandidate-shaped detail, for the detail panel

POST /api/console/disk/worktrees/clear
  body: { ids: [...], dry_run: bool }
  -> { ok, dry_run, removed: [{id, path, bytes_reclaimed}],
       failed: [{id, path, reason}], skipped: [{id, reason}] }
```

Each route proxies to trusty-mpm through the new MCP tool §16.4 names:
`disk/tree` reads the merged-PR survey in report mode plus a byte walk per
worktree; `worktrees/clear` with `dry_run: true` re-renders that same survey
as a confirmation list with no deletion; `worktrees/clear` with
`dry_run: false` runs the one and only delete path, which re-checks each
candidate via `recheck_before_delete` immediately before removing it.

### 16.6 Issue breakdown

Sized S/M/L, in the dependency order §17's Sequence carries as placeholders:

1. **Size index / cached byte walk** (M-L) — a background or on-demand cache
   mapping worktree path to bytes-with-timestamp, refreshed incrementally
   rather than walking cold on every load. Prerequisite for everything below.
2. **MCP tool for the merged-PR survey, plus the keep-list gate** (M) — a
   `disk_survey`/`worktree_survey`-shaped MCP tool on trusty-mpm returning
   per-worktree `ReclaimVerdict` and reason, and the owner keep-list as a new
   gate in `classify()`. Depends on 1 for bytes in the response.
3. **The console Disk view, display only** (M) — the sunburst component, the
   narrow-width list fallback, and the two `GET` routes in §16.5. No delete
   action in this step, per §13's rule.
4. **The Disk dashboard, the clear action** (M) — the `POST` route
   (dry-run and real, re-check-before-delete), the confirm-flow UI, and the
   audit-log attribution field §16.4 found missing. Depends on 3 for the list
   UI it reuses and 2 for the delete-capable tool call.

## {#SPEC-UNIDASH-17~draft} 17. Sequence

Ordered by dependency, not by priority alone. This amendment itself is
tracked on the epic, item 1.

1. **Epic [#6922](https://github.com/bobmatnyc/trusty-tools/issues/6922) —
   console/dashboard unification.** Tracks this DOC-73 amendment and every
   item below.
2. **[#6923](https://github.com/bobmatnyc/trusty-tools/issues/6923) —
   `trusty-search` tab cleanup.** Strips the inline MANAGE controls from
   `SearchTab.svelte` (§13, items 1-2), wires its row click into the existing
   `/tools/search` indexes view (§14).
3. **[#6155](https://github.com/bobmatnyc/trusty-tools/issues/6155) —
   `trusty-memory` and `trusty-analyze` UI migration into console** (existing
   issue, reused). Migrates `crates/trusty-memory/ui` into console and adds
   the palace management actions it does not have today (§12, §14), and
   migrates `crates/trusty-analyze/ui` into console (§12), including wiring
   `AnalyzeTab.svelte`'s row click — no control removal needed there, since
   that tab is already DISPLAY-only (§13).
4. **[#6928](https://github.com/bobmatnyc/trusty-tools/issues/6928) —
   `trusty-memory` tab cleanup** (after #6155). Strips `MemoryTab.svelte`'s
   inline `CompactAction`/`DeleteAction` (§13, item 3) once the migrated
   dashboard carries them, wires the row click (§14).
5. **[#6924](https://github.com/bobmatnyc/trusty-tools/issues/6924) — mpm
   dashboard, ported into console** (after #6460). Ports `trusty-mpm-gui`'s
   `Dashboard.svelte` and `SessionDetail.svelte` into a console-hosted
   `/tools/mpm` page (owner ruling, 2026-09-06 — "Port it into the console";
   §13, item 5) — the same shape as `/tools/search` and the #6155
   migrations, and not a Tauri app launched from console. Once it lands,
   strips `SessionsTab.svelte`'s inline Spawn/Resume/Stop/Decommission/
   bulk-delete controls and wires each row to `/tools/mpm/sessions/<id>`
   (§14). Sequenced after #6460 so the ported dashboard uses live push
   rather than another poller.
6. **[#6931](https://github.com/bobmatnyc/trusty-tools/issues/6931) — agent
   roll-up in the Sessions view** (blocked by #6924). Adds the per-session
   subagent roll-up — state glyph, agent type, one-line activity, elapsed
   time, tokens consumed — and the waiting-for-user-input signal, to
   console's Sessions view and the ported `/tools/mpm` page (§13, item 5).
7. **[#6925](https://github.com/bobmatnyc/trusty-tools/issues/6925) —
   activity pulses, read/write/index** (after #6460). Wires `trusty-search`
   and `trusty-memory` as `PushClient` producers, adds the `Index` kind, and
   lands the render points and the Foundry pulse token (§15).
8. **[#6926](https://github.com/bobmatnyc/trusty-tools/issues/6926) —
   incremental directory-size index** (§16.6, item 1).
9. **[#6927](https://github.com/bobmatnyc/trusty-tools/issues/6927) — MCP
   tool exposing the worktree survey/`classify` plus the keep-list gate**
   (§16.6, item 2). Depends on #6926.
10. **[#6929](https://github.com/bobmatnyc/trusty-tools/issues/6929) —
    console Disk view, sunburst** (§16.6, item 3). Depends on #6926 and #6927.
11. **[#6930](https://github.com/bobmatnyc/trusty-tools/issues/6930) — Disk
    dashboard, clear with dry-run** (§16.6, item 4). Depends on #6929.
