# 0065. Run the trusty-events bus as its own supervised daemon

- **Status:** Proposed
- **Date:** 2026-10-02
- **Scope:** Workspace-wide — concretely the new `trusty-events` crate,
  `trusty-console`, `trusty-common::control_bus`, `trusty-installer` (`tctl`),
  `trusty-mpm` (`tm doctor`), and every event producer (`trusty-mpm`,
  `trusty-code`, `trusty-agents`, `trusty-analyze`, `trusty-search`,
  `trusty-memory`)
- **Reversibility Cost:** Low today, rising with each producer — no producer
  pushes and no reader consumes the bus yet (#6849, #6850, #6851, #6854 open).
  Decision 2 below keeps the cost low after they land by naming the socket
  after the bus instead of after its host.
- **Decision Drivers:** owner ruling 27 (2026-10-02); the 2026-09-05
  non-blocking invariant; ADR-0058 (trusty-code is an independent product);
  ADR-0019 (durable acknowledged messaging rides the bus); ADR-0043
  (registry installs only); install and supervision cost per added binary
- **Supersedes / Superseded by:** Supersedes DOC-73 §4.1's placement ruling
  (console-hosted, 2026-09-05) and its rejection of a separate daemon
  (Option B). No earlier ADR is superseded; see Related Decisions.
- **Tracking:** #9077 (this ADR), epic #9074, #9075, #9076, #9078

## Context

### The ruling and the open question

Owner ruling 27, Bob, 2026-10-02 14:27Z:

> "My feeling is that events are important enough and are used by all the
> other demons, so it should be its own crate. But feel free to challenge that
> assertion. I'm happy to change previous ADRs and specs to make this right,
> but it seems like events are becoming a first-class system."

The crate is settled: epic #9074 moves the bus core into `crates/trusty-events`
(#9075) and makes the console a UDS client (#9076). This ADR decides the
remaining question: which **process** runs that crate.

Terminology: in this ADR "tm daemon" means the `trusty-mpm daemon` process
(launchd label `com.trusty.mpm`). The workspace alias table maps `tm` to
`trusty-memory`; that crate is always written out here.

### History of the placement

| Date | Ruling | Host |
|---|---|---|
| 2026-07-18 | "Single hub on the tm daemon" | trusty-mpm daemon |
| 2026-09-05 | "The only event bus actually." (DOC-73 §4.1) | trusty-console |
| 2026-09-05 | "Shouldn't block functionality, just messages and observability" | invariant, any host |
| 2026-10-02 | Ruling 27: events are a first-class system, own crate | open (this ADR) |

DOC-73 §4.1 (line 468) rejected "a new crate, a fourth daemon" because it
"adds a supervision target, an install step, and a failure mode for no
capability the existing crates lack."

### What exists in-tree, measured 2026-10-02 on `origin/main` 38dbffd9fa

- **Bus core, in console.** `crates/trusty-console/src/event_bus/`: `bus.rs`
  (ring default 8192, `EventId` dedup, console-assigned `seq`, `broadcast`
  subscriber API), `ingest.rs` (UDS NDJSON ingest, at most 256 concurrent
  connections), `log/` (day-rotated NDJSON, 7-day retention, seq recovery on
  start). About 1,670 lines in non-test files plus 1,170 test lines.
- **Wired at console start.** `crates/trusty-console/src/lib.rs:515-575` binds
  the ingest socket, opens the log, and serves ingest. A failed bind or log
  open degrades to a warning, per the non-blocking invariant.
- **Socket.** `daemon_socket_path("trusty-console")`
  (`event_bus/ingest.rs:105`), so the bus occupies the console's canonical
  socket name. `docs/reference/threat-model.md:115` records it.
- **Producer client.** `trusty-common/src/control_bus/push_client.rs`: a
  bounded buffer (4096 frames, 8 MiB), synchronous infallible `send`, one dial
  per `flush`, 500 ms budget, oldest-first drop with a `dropped` count.
- **No traffic.** No crate outside console and `trusty-common` calls
  `PushClient` (#6849, #6854 open). No route reads the ring (#6850, #6851
  open). `trusty-mpm`'s peer-bus RPC methods were removed in #6288 slice A
  (`crates/trusty-mpm/src/daemon/rpc/registry.rs:72-80`).
- **Streaming UDS exists.** `trusty-common/src/uds/server/stream.rs`,
  `uds/stream_client.rs` and `uds/sse.rs` already carry a multi-frame UDS
  response and render it as browser SSE. A console that reads the bus over UDS
  needs no new transport primitive.
- **Idle accounting counts open connections.**
  `trusty-common/src/uds/server/idle.rs` holds an on-demand service resident
  while any connection is open; #6621 showed a 15 s health poll kept an
  on-demand `trusty-analyze` resident for 46 h.

### Who uses the bus besides the dashboard

- ADR-0019: durable, acknowledged cross-agent messaging
  (Queued → Delivered → Processed) runs on the bus.
- 2026-07-18 ruling: the coding supervisor becomes a bus subscriber instead of
  a poller.
- DOC-73 §15.2: `trusty-search` and `trusty-memory` become producers.
- #9078: `trusty-analyze` pushes LSP events.

The bus is therefore daemon-to-daemon infrastructure, not only a console
feature. That is the substance of "first-class" in ruling 27.

### Host observations (one sample, this machine, 2026-10-02)

- `trusty-console` process started 2026-09-24; the trusty-mpm daemon started
  2026-10-01 16:09. `~/Library/LaunchAgents/` holds seven backup copies of
  `com.trusty.mpm.plist` and none of `com.trusty.console.plist`. The trusty-mpm
  daemon is the most frequently changed and restarted daemon (dev lane
  live-checks the debug build, per CLAUDE.md).

## Options

- **(A) Own daemon.** A `trusty-events` binary under launchd
  (`com.trusty.events`, `KeepAlive`). Every other process is a client.
- **(B) Library in the tm daemon.** The trusty-mpm daemon links
  `trusty-events` and serves it. Other daemons are clients over a socket the
  tm daemon owns.
- **(C) On-demand process** (ADR-0034 / ruling 24 pattern). Spawned on first
  call, exits when idle.
- **(D) Library in trusty-console** (added: today's host, kept as the
  comparator). Console links `trusty-events` and serves it.

## Comparison

| # | Criterion | (A) Own daemon | (B) In tm daemon | (C) On-demand | (D) In console |
|---|---|---|---|---|---|
| 1 | Host restart or crash | Only a bus bug restarts the bus | Every tm restart drops subscribers and the ring | Each idle exit drops the ring | Every console restart drops subscribers and the ring |
| 2 | Startup order | None | Console and Code reads need tm running | None; first event pays spawn and log recovery | None |
| 3 | Install and supervision (ADR-0043) | +1 published binary, plist, tctl member, doctor row | None new | +1 published binary and a spawner; no plist | None new |
| 4 | Socket owner and name | trusty-events, `trusty-events.sock` | tm daemon, second socket | trusty-events, `trusty-events.sock` | console; today `trusty-console.sock` |
| 5 | "Internal 0.x" holds | Rust API: yes. Wire: no, versioned in trusty-common | Same | Same | Same |
| 6 | Durability and replay | Own log dir; same recovery code | In tm data dir | Log replay on every spawn | Console data dir (today) |
| 7 | Cost to reverse | Low, if the socket keeps its name | Medium: tm RPC habits leak into clients | Low | Low, if renamed per decision 2 |

### 1. Failure isolation

The non-blocking invariant already protects **producers** in every option:
`PushClient` buffers 4096 frames through any host restart and replays on
reconnect. The options differ in what happens to **subscribers**, to the
in-memory ring, and to the host's own work.

- **(A)** A tm or console restart does not touch the bus. Subscribers that
  are not the restarted process stay connected. A bus crash loses only the
  ring; the log survives and `recovery.rs` resumes `seq`.
- **(B)** Every tm restart drops every subscriber, including the console
  stream and any ADR-0019 recipient, and empties the ring. The reverse risk is
  worse: process-wide failures (memory growth in the ring, file-descriptor
  exhaustion at 256 ingest connections, a full disk under the log) become tm
  daemon failures. That turns a bus defect into a session-control outage,
  which is exactly what the 2026-09-05 invariant forbids.
- **(C)** Idle exit empties the ring. While any subscriber holds a stream open
  the process never idles (`idle.rs` counts open connections), so (C) runs as
  (A) whenever console is up, with a spawner added.
- **(D)** A console restart (UI and route releases) drops every subscriber.
  Console's own work is observability, so the blast radius is smaller than
  (B), but ADR-0019 messaging between two agents would stall on a UI deploy.

### 2. Startup order

No option needs producers to start after the bus, because `send` never
dials. For **readers**: under (B), the console's stream and the trusty-code
and trusty-agents event paths need the trusty-mpm daemon running. ADR-0058
makes trusty-code a product that does not depend on trusty-mpm, so (B)
reintroduces that dependency for events. `tctl`'s
`dependency_graph.rs:68-69` would also need a console → trusty-mpm edge. (A),
(C) and (D) need no edge.

(C) has a spawn question that (A) does not. A producer may not spawn the bus,
because the invariant bars a producer from waiting on it. The spawner would
have to be console (ADR-0034's `UdsServiceSupervisor`), which ties the bus's
life to console's and makes (C) a slower (D).

### 3. Install and supervision cost under ADR-0043

(A) adds, once:

- a crates.io release of `trusty-events` with a binary, installed by
  `cargo install trusty-events --locked` (registry only, ADR-0043);
- a launchd plist `com.trusty.events` written by `tctl`'s
  `plist_bootstrap.rs`, plus a `stable_set` member and a `uds_socket_for` row
  in `probe_http.rs:171`;
- no Developer ID signing target unless the binary reads a TCC-protected
  path; it writes only under its own data dir, so `tctl sign` is not
  required;
- one `tm doctor` row (socket answers, binary provenance). `daemon/doctor.rs`
  sits at the 500-line cap, so the row ships as its own `doctor_*.rs` module,
  as the existing split-out rows do;
- one row in console's ADR-0035 health aggregate.

(B) and (D) add none of this. (C) adds the binary, the release, and the
provenance row, and replaces the plist with spawner code.

### 4. Socket ownership and naming

The bus today binds `daemon_socket_path("trusty-console")`. That name says
"console's API", so console cannot later serve any other UDS method on its own
name without multiplexing two framings (NDJSON ingest and JSON-RPC) on one
socket. Under (B) the same collision applies to the tm daemon's existing
JSON-RPC socket (`crates/trusty-mpm/src/daemon/socket.rs`), so (B) needs a
second tm-owned socket. Under (A) and (C) the bus owns
`daemon_socket_path("trusty-events")` =
`<data dir>/trusty-events/trusty-events.sock`, bound with
`bind_singleton_hardened`, `0600`, peer-uid checked (ADR-0034 §3).

### 5. Does "internal 0.x, no stability promise" still hold?

For the **Rust API**, yes, in (A) and (C), provided no other crate links the
`trusty-events` library: every client speaks UDS. In (B) and (D) the host
links it, which also holds, because the host is in the same workspace and
moves in lock-step.

For the **wire protocol**, no, in every option. Independently installed
binaries (an older `trusty-mpm`, a newer bus) meet on the socket. Version skew
is a real case once one binary ships per crate. The promise therefore moves:
the envelope, the frame and the subscribe method are the contract. They live
in `trusty-common::control_bus` (published, 0.52.x) and evolve under DOC-73
§3.3's versioning rule. `trusty-events` stays an implementation of that
contract with no Rust-API promise. #9075's "wire contract stays in
`trusty-common::control_bus`" already encodes this.

The roadmap's reason for publishing (`docs/roadmap/trusty-mpm.md:46-47`,
"only because the published crates that host it must be") holds for (B) and
(D). For (A) and (C) the reason changes: the crate ships its own binary, and
ADR-0043 permits only registry installs.

### 6. Durability and replay

DOC-73 §4.3 requires the day-rotated NDJSON log and `since_seq` replay
(#3157's durable-log requirement). ADR-0019 requires message durability and
resend on reconnect. The existing `log/` module satisfies DOC-73 in any host;
only the directory changes. (C) pays recovery on every spawn and serves
`since_seq` from disk until the ring refills. No design doc requires
cross-host durability (DOC-73 §10 Q3 leaves federation open).

### 7. Cost of reversing later

Clients bind to two things: the socket path and the wire contract. If both
are independent of the host, moving the host is a server-side change.
(A) → (D) means console links the library and binds `trusty-events.sock`,
then the plist, `tctl` member and doctor row are deleted; no client changes.
(B) is the costly one to leave: a bus inside tm invites clients to reach it
through tm's JSON-RPC router, the way `mpm.bus.*` did before #6288 slice A
removed it. Today all four options are cheap to reverse because no producer
or reader exists.

## Decision

We will run `trusty-events` as its own launchd-supervised daemon (Option A),
under four rules.

1. **One crate, one binary.** `crates/trusty-events` holds the bus core moved
   from `trusty-console/src/event_bus/` (#9075) and a `trusty-events serve`
   binary. No other crate links its library.
2. **The socket is named after the bus.** It binds
   `daemon_socket_path("trusty-events")`. Producers and readers resolve that
   path through one `trusty-common::control_bus` helper, never a host name.
   The console's own socket name is freed.
3. **The wire contract lives in `trusty-common::control_bus`.** Ingest frame,
   envelope, and a streaming `events.subscribe { since_seq, filters }` method
   built on the existing `uds/server/stream.rs`. `trusty-events` carries no
   stability promise for its Rust API; the contract carries DOC-73 §3.3's.
4. **The non-blocking invariant is unchanged.** No producer gains a runtime
   dependency, a startup edge, or a spawn path. `tctl` installs
   `trusty-events` with any producer as a soft member, not through
   `dependency_graph.rs`'s hard `requires` edges.

Console becomes a UDS client that relays the subscription to browsers over
the existing `uds/sse.rs` bridge (#9076). The log starts fresh under
`<data dir>/trusty-events/event_log/`; console's `event_log/` holds no
production traffic and is retired, not migrated.

### Where this agrees and disagrees with ruling 27

The ruling is right that the bus deserves its own crate and its own process,
but the strongest reason is not the one "important enough" suggests. Failure
isolation for producers is already solved by `PushClient`'s buffer in every
option, so a separate process does not make producers safer. What it buys is
neutrality and containment: the bus serves trusty-code and trusty-agents,
which ADR-0058 keeps independent of trusty-mpm, and it carries ADR-0019
messaging that should not stall on a console UI deploy or a tm dev-lane
restart. A bus defect also stays out of session control. Against that, the
cost is real (a binary, a plist, a `tctl` member, a doctor row) and DOC-73
§4.1's objection was fair when the bus had one consumer. "First-class" should
mean first-class ownership, never first-class dependency: the bus must remain
optional to every daemon's core function. Option B, the tm daemon, is the
weakest of the four: it reverses the 2026-09-05 move for no gain and makes
the most-restarted daemon the host.

### Why not (C)

A bus with a persistent subscriber never idles (`idle.rs`), so idle exit
saves nothing while console runs. Producers may not spawn it. The only
eligible spawner is console, which collapses (C) into (D) plus a process hop.
Ruling 24's on-demand model fits request/response services such as secrets,
not a stream. launchd socket activation (launchd holds the socket and starts
the binary on first connect) remains a later refinement of (A) that changes
no client.

## Consequences

**Easier**

- tm and console restarts no longer reset the bus's subscribers or ring.
- trusty-code, trusty-agents, trusty-search and trusty-memory publish to a
  host that is none of their peers.
- Console's socket name is free for console's own UDS API.
- Moving the host later touches only the server side (decision 2).

**Harder**

- One more published crate, binary, plist, `tctl` member, doctor row and
  health row to keep correct under ADR-0043.
- Console needs a UDS subscription it did not need in-process; #9076 grows
  from "read the ring" to "relay a stream".
- Wire version skew between independently installed binaries becomes a live
  case; DOC-73 §3.3's versioning rule has to be enforced by a test.
- A machine without `trusty-events` running has no events. Producers still
  work and count drops; the dashboard shows the disconnected state (§8.5).

**Risks**

- If the console's 15 s poller probes `trusty-events` with an answered method,
  it costs one RPC per poll; use the liveness-marked probe (#6621).

## Documents to amend on acceptance

Per DOC-46 §4, an earlier ADR's Context/Decision/Consequences are not edited;
specs and references are.

| Document | Section | Change |
|---|---|---|
| `docs/specs/DOC-73-unified-mpm-code-agents-dashboard.md` | header "Subsystem" (lines 12-21) | bus owner becomes `trusty-events` |
| same | §4.1 (lines 461-543) | table (DOC-73's own lettering): Option B "new crate" becomes "Decided", Option A "console-hosted" becomes "Superseded by ADR-0065"; keep the 2026-09-05 quotes as history; ownership boundary: console keeps dashboard code, not the bus core |
| same | §4.2 (lines 545-595) | ingest socket `daemon_socket_path("trusty-events")`; "console is the only ingester" → trusty-events |
| same | §4.3 (lines 597-644) | title "all console-side" → "all bus-side"; log dir |
| same | §4.4 (lines 646-687) | console routes read the `events.subscribe` stream |
| same | §10 Q1 (lines 1133-1140) | answer revised by ruling 27 and this ADR |
| same | §11 (lines 1181-1222) | slice 3 points at #9075; add #9076 |
| same | §15.2 (lines 1399-1418) | "console's ingest socket" → trusty-events socket |
| `docs/specs/README.md` | DOC-73 row (line 92) | subsystem names `trusty-events` |
| `docs/roadmap/trusty-mpm.md` | release 3 (lines 22, 42-47) | publish reason: own binary under ADR-0043 |
| `docs/reference/threat-model.md` | line 115 | row moves from console to `trusty-events` |
| `docs/architecture/port-assignments.md` | socket rows | add `trusty-events.sock` |
| `docs/reference/crate-map.md` | crate table | add `trusty-events` |
| `docs/adr/INDEX.md` | table | add 0065 |
| issues #9074, #9075, #9076 | bodies | name the host process and decision 2's socket |

`docs/research/shared-telemetry-over-uds-2026-09-19.md` (lines 23, 52) is a
dated research record and is not edited.

## Related Decisions

Vetted against `docs/adr/INDEX.md` and the ADRs below on 2026-10-02:

- **ADR-0004 (Three event-driven harnesses):** Consistent. The harnesses
  still share event infrastructure; its home is now a crate and a process.
- **ADR-0005 (Shared harness event bus):** Consistent. Its envelope stands in
  `trusty-common::control_bus`; its in-process `trusty-agents-common` bus was
  already scheduled for retirement by DOC-73 §4.1 (#6854). No status change.
- **ADR-0011 (tctl owns service lifecycle):** Extends. `tctl` gains a
  supervised member.
- **ADR-0019 (Unified IPC messaging on the event bus):** Consistent, and a
  driver: its durability and acknowledgment run in a host no UI deploy or tm
  restart interrupts.
- **ADR-0031 / ADR-0032 (UDS inter-crate; console is the only HTTP surface):**
  Consistent. `trusty-events` binds no TCP port; console stays the only HTTP
  surface and relays the stream.
- **ADR-0034 (on-demand supervised process):** Consistent. Its pattern was
  considered (Option C) and does not fit a stream with persistent subscribers.
  Its socket rules (§3) apply to the new socket.
- **ADR-0035 (console aggregates health over UDS):** Extends. One aggregate
  row added.
- **ADR-0043 (cargo bin policy, Proposed):** Consistent. It forces the
  registry publication this ADR records.
- **ADR-0058 (trusty-code is independent):** Consistent, and a driver: it
  rules out a trusty-mpm host (Option B).

No conflict with an Accepted ADR. The decision this ADR reverses is a spec
section (DOC-73 §4.1), not an ADR.
