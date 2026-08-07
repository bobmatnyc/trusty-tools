# 0031. Transport by purpose: inter-crate same-host traffic over UDS; all external traffic through one shared HTTP server

- **Status:** Proposed
- **Date:** 2026-08-06
- **Scope:** Workspace-wide (the tool path of every trusty-\* daemon:
  trusty-search, trusty-memory, trusty-analyze, trusty-review,
  trusty-bm25-daemon, trusty-embedderd, trusty-mpm)
- **Reversibility Cost:** Medium — the protocol is already JSON-RPC on both
  sides, so no serialization contract changes; what raises the cost is that
  loopback HTTP is currently the substrate for the MCP bridges, the embedded
  UIs' data fetches, and the console proxy, so unwinding it touches several
  crates at once.
- **Decision Drivers:** an owner directive on transport for internal tool
  calls; an observed capability divergence between two of trusty-memory's
  recall routes; an HTTP error surfacing through a nominally-STDIO MCP tool;
  redb's cross-process exclusive `flock`, which makes single-process store
  ownership a correctness requirement.
- **Supersedes / Superseded by:** None. **Consistent with / Extends ADR-0018** —
  the same relationship ADR-0018 itself recorded with ADR-0017. ADR-0018 is not
  amended and its status is unchanged: it governs *bind-address reachability*,
  this ADR governs *which local transport one caller class uses* to reach an
  already-loopback-bound daemon. Per the ADR immutability rule (DOC-46 §4) this
  is a new record rather than an edit to ADR-0018.

> **Why Proposed and not Accepted.** The owner stated the topology as a
> direction and the UDS move as *"let's consider"* — an open evaluation, not a
> decision. Measurements taken afterward (recorded below) also contradict the
> performance rationale originally given for it, which moves the case onto
> different ground. Recording this as Accepted would assert a settled mechanism
> that is not settled. The **classification test** below is usable today
> regardless; what awaits sign-off is the mechanism and the sequencing.

## Context

### The rule, as stated

The owner's directive: *"All native internal tools should be STDIO (JSON-RPC)
for speed and low overhead. HTTP is for management and cross functional use."*
Prompted by two instances — *"native memory recall should be STDIO, not http"*
and *"same with search."*

### What the topology actually is today

**This is not what exists, and the gap matters.** The honest description of
today's architecture:

> One long-lived daemon per service owns the state and speaks JSON-RPC over
> loopback HTTP. The stdio surface is a thin client of that daemon, not an
> independent path.

For trusty-memory this is explicit and deliberate. Since **#1078**,
`trusty-memory serve --stdio` is a **pure proxy**: "every JSON-RPC request is
forwarded to `POST /rpc` on the running daemon; the stdio process never opens
redb" (`crates/trusty-memory/src/commands/serve_stdio_bridge.rs:1-8`). trusty-search
and trusty-analyze have the same shape — `McpServer::new(base_url)` makes the
MCP server an HTTP client of the daemon it fronts
(`crates/trusty-search/src/mcp/`, `crates/trusty-analyze/src/mcp/http_client.rs`).

So both of trusty-memory's recall surfaces terminate in the same daemon, the
same `AppState`, and the same warm embedder:

```
hook today:  prompt-context (CLI) → GET /api/v1/palaces/{slug}/recall → MemoryService::recall → no fusion
MCP client:  serve --stdio (proxy) → POST /rpc → tools::dispatch        → handle_memory_recall → fusion
```

**What differs between them is the route, not the transport.**
`crates/trusty-memory/src/tools/mod.rs:102` maps `"memory_recall"` to
`handle_memory_recall`; the REST handler
(`crates/trusty-memory/src/web/recall_routes.rs:43`) reaches
`MemoryService::recall` instead. Choosing STDIO does not avoid HTTP — it adds a
process spawn in front of it.

### The measurements

Taken against a live daemon:

| Path | Latency |
|---|---|
| Loopback HTTP floor, warm | 0.26–0.6 ms |
| trusty-memory recall | 1.22 ms |
| trusty-search hybrid search | 7.72 ms median |
| `prompt-context` hook, end to end | ~30 ms — dominated by process spawn; HTTP is **under 2%** |
| Spawning `serve --stdio` per invocation | +10 ms |

Both stdio bridges pool one HTTP client per process, so the real path is already
warm — the loopback floor above is what a tool call actually pays, not a
cold-connection figure.

**Performance does not justify this change, and this ADR does not rest on it.**
The transport overhead being targeted is a fraction of a millisecond against
work that costs 1.2–7.7 ms, and in the hook's case it is under 2% of a budget
dominated by process spawn. The naive STDIO route is the slowest option
measured. Stating this plainly matters more than preserving the original
rationale: the case for changing transports has to be made on **failure
surface, port lifecycle, and access control**, which is where the Decision
below places it.

### The two symptoms, and what actually caused them

**An HTTP error leaked through an MCP tool.** A `mcp__trusty-search__search`
call failed with `POST http://127.0.0.1:7878/indexes/…/search 404 Not Found:
{"error":"unknown index: …"}`. An HTTP status code has no business in an MCP
response. It appears because the MCP server is an HTTP client — a layering leak,
which is real, but a leak of error translation rather than evidence that the
transport is too slow.

**The recall surfaces diverged in capability.** trusty-memory's MCP route fuses
a BM25 lane into vector results (`fuse_bm25_into_recall`,
`crates/trusty-memory/src/tools/memory_ops.rs:484`); the REST route the
per-prompt hook calls (`fetch_palace_recall`,
`crates/trusty-memory/src/commands/prompt_context/fetch.rs:52`) does not. So the
hot path got the weaker implementation.

**The cause was routing, not transport** — two handlers over one store, drifting
independently. This is the failure the common-entry-point rule in `CLAUDE.md`
exists to prevent, and the fix is to converge the routes, which is available
without changing any transport.

**Separately, the BM25 lane has never been enabled at all.**
`TRUSTY_BM25_DAEMON=1` is set nowhere in the workspace, and there are zero BM25
index directories across 102 palaces. So the "better" path's advantage was
latent rather than realized. An engineer is enabling and backfilling the lane
now. This is the actual reason the two surfaces differ in observed behavior, and
it is not a transport problem in any part.

### What the stores permit, and why the daemon is not negotiable

This decides whether a per-client process is even an option, so it was verified
against upstream source rather than assumed.

- **redb takes a cross-process exclusive lock.** redb 2.6.3 acquires
  `flock(fd, LOCK_EX | LOCK_NB)` on open and returns
  `DatabaseError::DatabaseAlreadyOpen` on failure
  (`redb-2.6.3/src/tree_store/page_store/file_backend/unix.rs:37`). This backs
  trusty-memory (via `trusty-common`'s `memory-core`), trusty-search's corpus
  (`crates/trusty-search/src/core/corpus/open_failure.rs:87` classifies
  `DatabaseAlreadyOpen` as `Contention`), trusty-analyze, and trusty-agents.
- **redb's MVCC is an in-process property.** Its own summary — "MVCC support for
  concurrent readers & writer, without blocking"
  (`redb-2.6.3/src/lib.rs:39`) — describes behaviour *within* the process
  holding the flock.

**This refutes the tempting carve.** "Reads can run per-process, only writes need
serializing" would be a clean boundary if redb allowed cross-process concurrent
readers. It does not: a second process cannot open the file at all while the
daemon holds it. The read/write distinction is real and load-bearing *inside* the
owning process — it is exactly what lets one daemon serve many concurrent
recalls — but it cannot justify per-session reader processes.

The workspace has already run this experiment and reverted it. #1078's stated
motivation is precisely this: the previous `serve --stdio` opened redb directly,
"when the HTTP daemon holds the exclusive write lock the stdio process fell back
to a read-only snapshot, causing write failures and stale reads." The
snapshot-fallback machinery from #59 still exists
(`crates/trusty-common/src/memory_core/retrieval/handle.rs:171-186`) as a
degraded read path. #3992 records the cost of contention when it is not avoided:
five concurrent openers made the fifth wait 10 s, and one `memory_remember` was
observed to hang for 1800 s.

So the daemon's role — the owner's "multi-session buffering" — is a
**correctness** property, confirmed by measured failures, not a performance
nicety. With around 100 sessions registered on one machine, it is what makes
concurrent operation work at all.

### Why the principle still has force

The conflict between "STDIO for internal tools" and "the daemon buffers
multi-session access" rests on an equivocation. Read as *"each client spawns its
own server process that opens the store,"* the rule collides with the daemon and
is refuted by everything above. Read as what it says — a **transport** for tool
calls — it does not collide: **transport and store ownership are orthogonal.**
A shared daemon can speak JSON-RPC over a local pipe while remaining the single
process that holds the flock.

Two daemons here already do exactly that:

- **trusty-bm25-daemon** — per-palace subprocess, newline-framed JSON-RPC over a
  Unix socket, owning its index outright: *"the subprocess IS the writer lock"*
  (`crates/trusty-common/src/bm25_client.rs:1-8`). Concurrency is a single-owner
  worker fed by an mpsc channel — "the canonical 'mpsc channel is the lock'
  pattern" — coalescing writes in a 50 ms window
  (`crates/trusty-bm25-daemon/src/batch_queue.rs:1-19`).
- **trusty-embedderd** — the same design, which the BM25 daemon mirrors.

Both are shared, both buffer multi-session access, neither has an HTTP surface.

**And the UDS transport is not hypothetical here — it is live, in-tree, and
integration-tested.** `crates/trusty-common/src/embedder_client/uds.rs` already
carries newline-framed JSON-RPC 2.0 over `tokio::net::UnixStream` for
trusty-search ↔ trusty-embedderd, sharing the `EmbedderClient` trait so "call
sites are identical regardless of transport." Note it is **JSON-RPC directly
over UDS, not HTTP-over-UDS** — which is what makes `reqwest`'s missing UDS
transport (ADR-0018's first objection) irrelevant to this shape. That objection
was costed against tunnelling HTTP through a socket; nothing here does that.
This makes the change substantially smaller than ADR-0018 estimated.

*Doc/reality drift worth fixing separately:* `crates/trusty-memory/src/transport/rpc.rs`'s
module doc claims a UDS transport exists. It does not — `transport/mod.rs:15`
declares only `pub mod rpc;`, with no `uds.rs` sibling, and
`trusty-common/src/rpc/transport/` carries `http` and `stdio` only. Designed-for,
not implemented. Do not cite it as precedent.

### Why UDS rather than STDIO — the #1078 argument

This is the strongest argument in the record, and it is about correctness rather
than speed.

Per-session STDIO processes doing real work is the **abandoned** architecture.
#1078 abandoned it for cause: the stdio process opened redb directly, and when
the daemon held the exclusive write lock it fell back to a read-only snapshot,
"causing write failures and stale reads." UDS carrying JSON-RPC to one resident
daemon **preserves everything #1078 bought**; per-process STDIO gives it back.

So of the two readings of the owner's directive, only one survives contact with
this history. "Local pipe" must mean a socket to the resident daemon, never a
spawned per-caller process.

### The daemon is also the shared-state host

Beyond the write lock, the daemon holds warm state that no per-caller process
could reproduce cheaply: trusty-search keeps permanently-resident HNSW graphs, a
256-entry shared embedding cache, and one embedder subprocess — **measured RSS
1775 MB across 28 indexes.**

Under any outcome here the daemon's role is **management surface plus
shared-state host**, not management only. That is worth stating because
"HTTP is for management" could otherwise be misread as demoting the daemon to a
control plane.

## Decision

We will treat **transport as a function of a path's purpose**, not of the daemon
that happens to host it. The owner has since stated the end state as a topology,
which is the clearer form:

> **Inter-crate traffic, same host, travels over UDS. All external traffic goes
> through one shared HTTP server.**
>
> Exactly one component speaks HTTP outward. Every daemon speaks UDS inward.
> Nothing else has a choice to get wrong.

The shared HTTP server is **trusty-console** — the family's one REST gateway
(Model A, 2026-06-30), already named by ADR-0011 and ADR-0018 as the sole
non-loopback surface.

Two owner confirmations fix the boundary:

- *"yes, same host only"* — UDS's same-host constraint is acceptable; no
  inter-crate link needs to cross a machine boundary.
- *"all external comms go through a shared http server"* — one external surface,
  not one per daemon.

**This collapses the earlier "management versus cross-functional" phrasing into
one category.** Management traffic and cross-functional traffic are both
*external* traffic, and both arrive through the shared server. There is no third
category, and this ADR does not invent one.

The daemon stays. A daemon owning a single-writer store remains the single
process that opens it; transport and store ownership are decided independently.

### What the decision rests on

Not performance — the measurements above rule that out. Three other grounds:

1. **Access control.** A loopback TCP port is reachable by **any local process**,
   including any other user's on a shared host. A `0600` socket in a
   user-owned directory is not. This does not weaken ADR-0018's goal; it
   **strengthens** it — the per-daemon bind posture and origin-guard reasoning
   collapse into a file permission, enforced by the kernel rather than by each
   daemon remembering to wrap its router.
2. **Port lifecycle.** Sockets have no port-allocation problem: no
   auto-selection across a range (trusty-memory scans 7070–7079 today), no
   collisions, no `http_addr` file to poll for a dynamic port, no stale-port
   discovery races.
3. **Failure surface.** One transport for inter-crate traffic instead of a
   loopback HTTP stack per daemon removes the layering leak that put an HTTP
   `404` inside an MCP response.

### Serialization: decided, not deferred

**No case for msgpack, bincode, or Cap'n Proto from the current data.** The
measurements show no marshalling bottleneck anywhere — JSON-RPC framing is not
what the 1.2–7.7 ms is spent on. This is recorded as settled so it is not
reopened as a speculative optimization; revisit only if a future measurement
shows serialization cost that matters.

Two things this decision explicitly does **not** settle, recorded as open:

- **Whether to actually adopt UDS.** The owner's words were *"let's consider
  switching to UDS"* — an **open evaluation, not a decision**, owner-initiated
  and currently unassigned. The topology above names UDS because that is the
  mechanism under evaluation; nothing here commits to it.
- **Sequencing.** Given the measurements, converging the divergent recall routes
  and enabling the BM25 lane are independently valuable and available now.
  Migrating transports is neither urgent nor justified by latency, and the two
  are separable — the routing defect is fixed by choosing a handler, not a
  transport.

### The UDS scope condition

Though adoption is open, one thing about it *is* settled, recorded here so the
evaluation starts from it rather than relitigating it:

> **If we go to UDS, we do it for all inter-crate transport.** All-or-nothing
> across every inter-crate link, never a per-crate opt-in.

The reason generalizes past this decision: **a mixed fleet is worse than either
pure end state.** Half-migrated means two discovery mechanisms, two failure
modes, two threat-model rows per daemon, and a permanent ambiguity about which
crate does which — a standing tax on everyone who touches the fleet, paid to
reach neither destination.

Two consequences follow, and they change the shape of the work:

1. **It is one shared abstraction, not N migrations.** A listener/dial module in
   `trusty-common` that every daemon mounts, rather than per-crate socket code.
   Direct precedent: the universal `config` subcommand, implemented once in
   commons and mounted by all primary crates (owner directive, 2026-07-11). The
   common-entry-point rule in `CLAUDE.md` requires this shape regardless — a
   second independent implementation of a shared capability is a defect there,
   not a convenience.
2. **trusty-console becomes the single translation boundary.** It keeps serving
   HTTP outward as the family's one REST gateway (Model A, 2026-06-30) and
   reaches daemons over UDS. Exactly one component speaks both, instead of every
   daemon carrying two listeners. That is the owner's own line — "HTTP is for
   management and cross-functional use" — expressed as topology rather than as a
   rule each daemon has to remember and can drift from.

**Both of ADR-0018's objections to UDS dissolve under this scope** rather than
being overridden — see Related Decisions for the full treatment. In short:
`reqwest`'s missing UDS transport was costed against tunnelling HTTP through a
socket, and this carries JSON-RPC directly; webviews cannot reach a socket, and
under this topology they never try, because they are external clients served by
the shared HTTP server.

**What stays HTTP by definition** — not exceptions to the condition, but outside
its scope, since UDS is same-host only:

- **Browser clients** — every embedded SPA, the console dashboard, Tauri
  webviews.
- **Anything cross-machine or cross-container.** A research agent is checking
  whether any such consumer exists today; **pending**.

### The classification test

Under the topology above the test reduces to a single question:

> **Is the caller outside this host, or another crate on it?**

- **Another crate on this host → UDS.** All of it, per the all-or-nothing scope
  condition. This covers every native internal tool call: agent sessions, CLIs,
  and sibling daemons reaching a daemon's domain capability.
- **Outside this host → the shared HTTP server.** This covers everything
  previously split across "management" and "cross-functional": browsers and
  webviews, off-machine clients, webhook senders, and console-proxied traffic.

**Browsers are external even when the browser is on this host.** UDS is not
reachable from a webview, so a browser client is external by capability rather
than by geography. This is the one place where the question's phrasing can
mislead, and it is the constraint that defeated ADR-0018's full-UDS
consideration.

The purpose-based framing that motivated this ADR — calls *through* a daemon to
its domain capability, versus calls *about* the daemon (status, health, port,
lifecycle, metrics, index administration) — is no longer a separate axis to
judge. It falls out: a sibling crate asking a daemon for status is still
inter-crate, still UDS; an operator asking through a browser is external, still
the shared server. Where the two framings would disagree, **the caller decides,
not the payload.**

### Applying the test to today's surfaces

**Side** is the classification under the test above. **State today** is what
exists — and for every daemon except the last two, the tool path currently runs
over loopback HTTP regardless of its classification.

| Surface | Side | State today |
|---|---|---|
| trusty-search MCP tools (`search`, `grep`, `search_semantic`, `search_similar`, `get_call_chain`, `typeahead`) | Inter-crate → UDS | stdio frontend, HTTP client to the daemon; source of the leaked 404 |
| trusty-search CLI `status`/`port` | Inter-crate → UDS | Loopback HTTP |
| trusty-search embedded UI | External → shared server | Served by the daemon's own HTTP today |
| trusty-memory recall/remember/note/list MCP tools | Inter-crate → UDS | `serve --stdio` is a **pure proxy** to `POST /rpc` (#1078); never opens redb |
| trusty-memory per-prompt recall hook | Inter-crate → UDS | Calls the REST recall route directly; the route lacking BM25 fusion |
| trusty-memory palace/wing/room admin, activity history | Inter-crate → UDS | Loopback HTTP; CLI and MCP callers |
| trusty-memory embedded UI | External → shared server | Served by the daemon's own HTTP today |
| trusty-analyze MCP tools | Inter-crate → UDS | stdio-over-HTTP client (`mcp/http_client.rs`) |
| trusty-analyze embedded UI | External → shared server | Served by the daemon's own HTTP today |
| trusty-analyze `POST /webhooks/github` | External → shared server | Direct, unproxied — already a known ADR-0018 gap |
| trusty-review MCP tools | Inter-crate → UDS | stdio path reuses the daemon's `AppState` and pipeline in-process (`mcp/mod.rs`) |
| trusty-review GitHub webhook | External → shared server | HMAC-verified external sender |
| **trusty-bm25-daemon** | Inter-crate → UDS | Unix-socket JSON-RPC, no HTTP surface — **already the target shape** |
| **trusty-embedderd** | Inter-crate → UDS | Unix socket + stdio, no HTTP surface — **already the target shape** |
| trusty-mpm MCP tools (33) | Inter-crate → UDS | Dual-wired: `dispatch` serves both `run_stdio_loop` and `POST /rpc` (`crates/trusty-mpm/src/mcp/mod.rs`) |
| trusty-mpm daemon API for the `tm` CLI and fleet supervision | Inter-crate → UDS | Loopback HTTP |
| trusty-mpm-gui (Tauri webview) | External → shared server | Talks directly to the daemon today (#3333) |
| trusty-console (all of it) | **The shared server itself** | HTTP — unchanged |

### The one genuine conflict: `index_file`, `remove_file`, `reindex`

**These endpoints have two caller classes at once, and the test resolves them
differently.** State this explicitly or someone will move the wrong one:

- As **MCP tools**, an agent on this host calls them → inter-crate → UDS.
- As trusty-search's **documented cross-host CI / build-box integration path**,
  a remote builder calls the same endpoints → external → stays HTTP, through the
  shared server.
- **trusty-mpm's `ensure_project_indexed` is a third caller** — cross-crate
  orchestration, unambiguously cross-functional → stays HTTP.

So these endpoints must remain reachable over HTTP **and** gain a UDS path; they
are the one place the two transports genuinely coexist by design rather than
during a migration. The caller decides, not the endpoint — which is exactly the
rule stated in the test above, here with real consequences.

### Surfaces this ADR does not classify

Named rather than guessed:

- **trusty-agents' per-project search-as-a-service daemon**
  (`crates/trusty-agents/src/search/service/mod.rs`, **#3335**). Undeclared in
  every inventory; ADR-0018 already flags it as needing declaration or
  retirement. Cannot be classified until that resolves.
- **trusty-agents' main daemon.** Outside the current five-crate core scope and
  mid-remediation under #3329 (still defaults to `0.0.0.0`, auth off).

## Consequences

**Easier / positive:**

- Gives a reader a test that places a new endpoint without a judgment call,
  which no existing document provided.
- Names the layering leak (HTTP status codes reaching MCP responses) as a defect
  with an owner, whichever transport is chosen.
- Records the measurements, so the next person to propose a transport migration
  starts from evidence rather than from the intuition that HTTP must be slow
  here.
- Separates two problems that were being solved as one: route convergence and
  BM25 enablement fix the observed defect; transport is a separate question with
  a much weaker case.

**Harder / negative / trade-offs:**

- **The stated rationale is contradicted by the measurements.** "Speed and low
  overhead" targets 2–7 ms, and the naive implementation costs 10 ms more. Any
  migration must be justified on other grounds — layering cleanliness, one
  implementation per capability — and this ADR does not claim those outweigh the
  cost. That case has not been made.
- **A migration is not a transport swap.** Because the stdio surfaces are HTTP
  clients rather than independent paths, moving to a local pipe means changing
  what the MCP servers *are*, not merely how they are framed.
- **The embedded SPAs and Tauri webviews cannot follow.** They require an
  `http://` origin, so every daemon shipping a UI keeps its HTTP server. Tool
  path and UI data path would diverge, relocating the drift risk rather than
  eliminating it — though more narrowly, since UIs are not per-turn hot paths.
- **Both transports would coexist during any transition**, which is the same
  divergence window that produced the original defect.

**Not yet verified — flagged rather than asserted:**

- **Stale-socket and crash-cleanup behaviour of the existing embedder UDS
  listener was not audited.** A socket left behind by a crashed daemon is the
  classic UDS failure mode, and the in-tree precedent's handling of it is
  unknown. This must be checked before the pattern is generalized, since a
  fleet-wide migration multiplies whatever the current behaviour is.
- **No Windows path handling was found either way.** Whether the socket paths
  work, or are gated off, on Windows is undetermined.
- **Whether any consumer reaches a daemon from off-host today** is being checked
  by a research agent; **pending**. If one exists it moves behind the shared
  server rather than becoming an exception.

**Unchanged:**

- Every HTTP route that remains is governed by ADR-0018 exactly as before:
  loopback bind, the shared `trusty_common::server` origin guard on
  write-mutating routes, and external reachability only via trusty-console's
  reverse proxy. This ADR authorises no new off-loopback binding.

## What this decision does **not** mean

- **Not "delete the HTTP servers."** Every daemon keeps its HTTP surface for CLI
  status, health, metrics, embedded UIs, the console proxy, and webhooks.
- **Not "retire the daemons."** The daemon is what makes concurrent
  multi-session operation correct against a single-writer store. #1078 already
  reverted an attempt to bypass it.
- **Not "one server process per session."** A shared daemon reached over a local
  socket satisfies this ADR, and is what trusty-bm25-daemon and trusty-embedderd
  already do.
- **Not "STDIO is faster."** Measured, it is not. See the table above.
- **Not a decision to adopt UDS.** That is an open owner-initiated evaluation.
- **Not a reversal of ADR-0018.** Loopback-only governs all remaining HTTP.

## Related Decisions

Vetted against `docs/adr/INDEX.md` and prior ADRs on 2026-08-06:

- **ADR-0018 (Loopback-only doctrine):** **Consistent / Extends** — the same
  relationship ADR-0018 recorded with ADR-0017. **ADR-0018 is not amended and
  its status is unchanged.** Verified against its decision text rather than
  assumed: ADR-0018's normative content is entirely about *bind-address
  reachability* — which daemon may bind off-loopback, the origin guard on
  write-mutating routes, console-proxy-only external reach. This ADR governs
  *which local transport one caller class uses* to reach an already-loopback-bound
  daemon. Different axes; no clause of ADR-0018 is contradicted, and nothing here
  authorises a new off-loopback bind.

  ADR-0018 did **evaluate and reject** a full-UDS migration, on two costed
  grounds. Both dissolve under this ADR's scope rather than being overridden:

  - *"`reqwest` has no UDS transport."* Costed against tunnelling **HTTP over
    UDS**. This ADR proposes **JSON-RPC directly over UDS**, the shape already
    shipping in `crates/trusty-common/src/embedder_client/uds.rs`, where no HTTP
    client is involved at all.
  - *"GUI webviews cannot `fetch()` a Unix socket."* Exactly the case the owner's
    scope condition routes to the shared HTTP server. Webviews are external
    clients; they never touch a socket.

  It also **strengthens ADR-0018's stated goal.** A loopback TCP port is
  reachable by any local process; a `0600` socket in a user-owned directory is
  not. The per-daemon bind-guard reasoning collapses into a kernel-enforced file
  permission, which is a stronger guarantee than each daemon remembering to wrap
  its router.
- **ADR-0011 (`tctl` owns service lifecycle; console owns the single HTTP
  surface):** **Consistent.** ADR-0011's point 2 — "Local services stay
  JSON-RPC-over-stdio (MCP)" — states the same principle this ADR makes
  decidable. ADR-0011 is already `Amended by 0018` and remains so; its status is
  not changed here. Recorded because the texts agree and a future reader
  comparing them should know that is not coincidence.
- **ADR-0014 (Ship full native MCP support):** **Consistent / Extends.**
  ADR-0014 consolidated the MCP framework in `trusty-common`; this ADR specifies
  which transport those servers' tool calls should use underneath.
  `trusty_common::mcp::run_stdio_loop` is already the shared loop.
- **ADR-0017 (Shared webhook ingress, Proposed):** **Consistent.** Webhooks are
  cross-functional by this ADR's test and stay HTTP.
- **ADR-0019 (Unified IPC messaging on the event bus):** **Consistent.**
  ADR-0019 governs cross-PM and cross-agent *messaging*; this ADR governs
  *tool-call* transport into a daemon. Different channels.
- **ADR-0012 (Per-instance GUID and marker-file identity):** **Consistent.**
  Index identity is orthogonal to transport. Noted only because the leaked 404
  was an index-identity error surfacing through the wrong layer; the identity
  question is ADR-0012's and is not reopened.

No conflicts with any other Accepted ADR. Summary: a narrowing amendment to
ADR-0018, consistent with ADR-0011's original statement of the same principle,
and no silent contradictions.
