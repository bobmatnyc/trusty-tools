# 0034. Webhook ingress: `trusty-console` terminates the HTTP request, verifies HMAC once, spools the payload durably, and relays over UDS to a console-supervised on-demand process — amends ADR-0032

- **Status:** Accepted
- **Date:** 2026-08-07
- **Accepted:** 2026-08-07 (owner ruling: "Console relays over UDS.")
- **Scope:** Workspace-wide, concretely `trusty-console`, `trusty-analyze`,
  `trusty-review`, and a new shared UDS listener/dial module in
  `trusty-common`
- **Reversibility Cost:** Medium — the relay hop and the spool are additive
  and removable; the residency mechanism is a generalisation of code that
  already exists (`Bm25Supervisor`). Undoing it means re-adding an HTTP
  listener to two crates, which is the state this ADR moves away from.
- **Decision Drivers:** owner ruling on webhook ingress; ADR-0032's
  explicitly-unresolved Open Question; milestone `tm 1.3.5` criterion (c);
  the measured cost of a cold `trusty-review` start (191 ms) against a
  median review (36.7 s) in
  [#5028](https://github.com/bobmatnyc/trusty-tools/issues/5028); the
  recurring fail-open shape this repo has shipped four times
- **Supersedes / Superseded by:** **Amends ADR-0032.** ADR-0032's Decision
  stays in force in full; this ADR closes the Open Question that ADR-0032
  deliberately left open. ADR-0032's Status is updated to `Amended by 0034`
  and its Context/Decision/Consequences are left as originally accepted per
  the ADR immutability rule (DOC-46 §4).

## Context

### The ruling

The owner, 2026-08-07:

> "Console relays over UDS."

`trusty-console` receives the external GitHub webhook and relays it inward to
`trusty-analyze` / `trusty-review` over a Unix domain socket. Neither target
keeps an HTTP listener. The owner rejected three alternatives explicitly: a
webhook-only HTTP listener on each service, a spawn-on-delivery mechanism
presented as a separate architecture, and deferring criterion (c) to 1.3.6.

### The problem the ruling creates, which ADR-0032 named

ADR-0032 stated the tension precisely and declined to resolve it:

> "If the target is on-demand (per #5028's undaemonizing goal), console
> relaying over UDS requires either the target process to be already running,
> or a spawn-on-delivery mechanism that does not yet exist in any ADR."

Relaying over UDS requires a bound listener at the far end. Criterion (c)
wants no always-resident analyze/review daemon. Both cannot hold unless
something makes the target resident at the moment of delivery. The owner
chose this path knowing that, so residency is now an implementation question.

### What already exists in-tree

**UDS transport: present three times, with no shared entry point.**

| Site | Framing | Socket path |
|---|---|---|
| `trusty-common/src/embedder_client/uds.rs` (`UdsEmbedderClient`) | newline-framed JSON-RPC 2.0, one request per connection, write half-closed | `$TMPDIR/trusty-embedderd.sock` |
| `trusty-common/src/bm25_client.rs` (`Bm25Client`) | identical, plus typed `Bm25RpcError` carrying the JSON-RPC code | `$TMPDIR/trusty-bm25-<palace>.sock` |
| `trusty-agents/src/ctrl/socket.rs` (`CtrlSocket`) | NDJSON | `~/.trusty-agents/sockets/<project>.sock` |

Server halves live in `trusty-embedderd/src/uds_server.rs` and
`trusty-bm25-daemon/src/socket.rs`. The framing convention is therefore
settled by precedent — newline-framed JSON-RPC 2.0 — but ADR-0032's
"a UDS listener/dial module in `trusty-common`, mounted by every daemon"
does **not** exist. Two of the three clients above are near-duplicates of
each other, which is the shape the common-entry-point rule exists to stop.

**🔴 Socket permissions are not what the prior ADRs claim.** ADR-0031 rests
its case on "a loopback port is reachable by any local process, a 0600 socket
is not," and ADR-0032 repeats it: "a `0600`-permissioned socket in a
user-owned directory replaces a loopback TCP port." No production code in
this workspace calls `set_permissions` or `PermissionsExt` on any socket —
every hit is in test fixtures. Sockets are created at the default umask
(commonly `0755`) and two of the three conventions place them in `$TMPDIR`,
falling back to a shared `/tmp` when `TMPDIR` is unset. On Linux that is a
world-writable shared directory. The claimed guarantee is aspirational, and
this ADR must not build a trust boundary on it without making it real.

**Spawn-on-demand supervision: present, hardened, and load-tested.**
`trusty-memory/src/bm25_supervisor.rs` (`Bm25Supervisor::ensure_running`)
already implements the whole mechanism ADR-0032 said "does not yet exist":
an external-mode opt-out, an already-supervised fast path, adoption of a
socket some other process already bound, a `spawn_gate` that serialises
spawns so concurrent callers cannot double-spawn, an LRU cap on live
children, an RSS ceiling compared against a real measurement, exponential
backoff while probing for the socket, `kill_on_drop`, SIGTERM-then-SIGKILL
with a patience window that exceeds the child's own flush budget, and
dead-child eviction with one restart. It carries the scars of #2845 and
#2846. Separately, `trusty-review` already invokes `trusty-analyze` as a
per-invocation subprocess (`SubprocessAnalyzeClient`, #632) rather than
requiring a resident sidecar.

**The two affected senders, and how they disagree.**

`trusty-review`, `src/service/webhook.rs:147-161`: an empty
`github_webhook_secret` produces `401` and rejects every delivery. Fail
closed.

`trusty-analyze`, `src/service/handlers/review.rs:135-158`: an absent secret
produces `tracing::warn!("no webhook secret configured — skipping webhook
signature verification")` and **processes the payload anyway**. Fail open.
Two implementations of the same guard, opposite answers.

**The fail-open shape, already shipped on this exact path.** Both handlers
return `202 Accepted` *before* the work runs, then `tokio::spawn` it and
downgrade every failure to a log line — `trusty-analyze`
`review.rs:210-214` warns and drops; `trusty-review` `webhook.rs:305-309`
warns and writes `last_error`. GitHub does not retry a delivery it has
already seen acknowledged, so a failure after the 202 loses the event with
every health signal still reporting fine. This is the same shape as
`ensure_project_indexed`, whose caller discards the result outright
(`trusty-code/src/run_task/mod.rs:127`: `let _ =
trusty_common::search_index::ensure_project_indexed(&project, true);`), and
the same shape as #5020's `in_flight` counter, measured pinned at 9 for ten
days on a live instance. Adding a relay hop adds one more place to drop the
payload. The design below is constrained by that, not merely aware of it.

**Volume, measured.** #5028 reports webhook deliveries over 14 days on the
production instance: **zero**. Review volume: 2.3/hour, peak 5.8/hour, max
observed concurrency 3, duty cycle 2.1%. Cold start: 191 ms. Median review:
36.7 s. Spawn cost is 0.5% of the work it precedes.

## Decision

We will implement webhook ingress as follows.

> **`trusty-console` is the only process that speaks HTTP to GitHub. It
> verifies the HMAC over the exact received bytes, writes the delivery to a
> durable spool before acknowledging, and only then relays it over UDS to the
> target. If the target is not resident, console starts it, using the
> supervision mechanism `Bm25Supervisor` already implements, promoted into
> `trusty-common`. The socket's filesystem permissions are the credential
> that the relay hop rests on, and they are enforced and tested rather than
> assumed.**

### 1. Residency: console supervises the target on demand

Console owns a `UdsServiceSupervisor` in `trusty-common`, generalised from
`Bm25Supervisor`. On a delivery for a target that is not answering its
socket, console spawns the target, probes with exponential backoff until the
socket accepts, and relays. The supervisor keeps the child for an idle
timeout and then reaps it, so the steady state is "not resident."

Alternatives, each with what it costs and whether anything in-tree does it:

| Option | Cost | Breaks | In-tree today |
|---|---|---|---|
| **launchd/supervisor-managed residency** | A resident process per webhook-bearing service, forever | Criterion (c) directly — this is the state it exists to leave | Yes — `tctl` owns service lifecycle (ADR-0011), launchd plists |
| **Console spawns the target (chosen)** | 191 ms added to a 36.7 s job; a supervisor to generalise and own | Nothing measured; needs #5067 and #5064 fixed first (below) | Yes — `Bm25Supervisor`, and `SubprocessAnalyzeClient` per-invocation |
| **Queue-and-retry, payload durably held, target drains when next up** | A durable queue plus a drain trigger; latency unbounded until something runs | Webhook-driven review stops being timely, which is its whole point | Partly — `trusty-agents/src/listeners/store.rs` `EventStore`; ADR-0019's acknowledged IPC |
| **Reject and let GitHub retry** | Nothing to build | GitHub does not auto-retry `pull_request` deliveries usefully; redelivery is a manual console action. Events are lost, silently | No |

**Chosen: console spawns the target.** It is the only option that satisfies
both the ruling and criterion (c) at once; the mechanism is already built and
hardened in-tree rather than invented here; and the measured numbers say the
cost is 0.5% of the work while the throughput case for residency does not
exist (zero deliveries in 14 days). Queue-and-retry is kept as the *failure*
path below, not the primary path — that is where its durability earns its
keep.

### 2. The payload on failure, and how a drop is detected

The relay must not inherit the ACK-then-drop shape. Therefore:

- Console returns `202` **only after** the delivery — raw body, headers,
  `X-GitHub-Delivery` GUID, and the verification result — is written and
  fsync'd to a spool under console's own state directory. If the spool write
  fails, console returns **5xx**, so GitHub records a failed delivery that
  remains redeliverable from its UI. An unacknowledged delivery is recoverable;
  an acknowledged-then-lost one is not.
- Relay failure — target will not spawn, socket never binds, target rejects
  the frame — leaves the spool entry `pending` with an incremented attempt
  count. It is never deleted on failure. Console retries with backoff.
- A spool entry is deleted only when the target has acknowledged the frame on
  the UDS response.
- **Detection.** Oldest-pending-age and failed-attempt-count are exported
  through console's existing `/api/console/metrics/*` surface. A pending entry
  older than a threshold is a **red** health state and a non-zero exit for
  `tctl doctor` — not a `warn!` line in a log nobody reads.
- 🔴 **Explicitly forbidden on this path:** `let _ = relay(...)`, a bare
  `tracing::warn!` as the sole record of a failed relay, and any `202` issued
  before the spool write returns. Those three are the mechanism by which
  `ensure_project_indexed`, #5020, and both current webhook handlers lose work
  while reporting healthy.
- The unset-secret policy is unified to `trusty-review`'s fail-closed `401`.
  `trusty-analyze`'s skip-with-a-warning is removed, not carried forward.

### 3. The trust boundary

**Verification happens exactly once, at console, over the exact bytes GitHub
sent.** This is forced rather than chosen: the HMAC covers the literal request
body, so any re-framing into a JSON-RPC envelope destroys the ability to
verify it. Console holds the secret; the targets do not.

**What the UDS hop trusts, stated precisely: the filesystem permissions on
the socket path, and nothing about the transport itself.** "It arrived over
UDS, so it is trustworthy" is the badly-named version of this boundary and
would be a security defect — as established in Context, these sockets are
*not* currently `0600`, and on a Linux host with `TMPDIR` unset any local user
can connect to one. Naming the socket as the credential obliges us to make it
one:

- The socket is created in a `0700` directory owned by the running user, under
  the service's own state directory — following `trusty-agents`'
  `~/.trusty-agents/sockets/` convention, **not** the `$TMPDIR`-with-`/tmp`-
  fallback used by the embedder and BM25 clients.
- The socket itself is `chmod 0600` after bind, before it accepts a
  connection.
- The target verifies peer credentials on accept (`SO_PEERCRED` on Linux,
  `getpeereid`/`LOCAL_PEERCRED` on macOS) and refuses any connection whose uid
  is not its own. This is what makes the permission bits an enforced boundary
  rather than a documented intention.

**What console proves to the target:** the relayed frame carries the raw body
verbatim, the original headers, the delivery GUID, and an explicit provenance
record — the algorithm checked (`hmac-sha256`), which key id was used, and the
result. The body is forwarded byte-exact rather than re-serialised so a target
retains the option of re-verifying independently. The target trusts the
provenance assertion because only a same-uid process could have written it
through a `0600` socket.

### 4. The transport

Newline-framed JSON-RPC 2.0 over `UnixStream`, matching the two existing
clients. What is new is a single shared listener/dial module in
`trusty-common` that every consumer routes through — the one ADR-0032 assumed
and the workspace does not have. The three current implementations are
migrated onto it rather than a fourth being added.

## Consequences

**Easier / positive:**

- Criterion (c) becomes buildable: `trusty-analyze` and `trusty-review` lose
  their HTTP listeners without losing webhook ingress.
- The HMAC check collapses from two divergent implementations to one, in the
  only process that can correctly perform it. The fail-open branch in
  `trusty-analyze` disappears rather than being duplicated a third time.
- The webhook path gains a durability property it has never had: today both
  handlers ack first and drop on failure. After this, a lost delivery is
  either recoverable from GitHub (never acked) or visible in a health signal
  (acked and spooled).
- ADR-0031's and ADR-0032's `0600` access-control claim becomes true. It is
  currently the load-bearing argument for UDS over loopback TCP and it is not
  implemented.
- The three duplicate UDS clients collapse onto one shared module, which the
  common-entry-point rule already required.

**Harder / negative / trade-offs — and the strongest argument against this
decision:**

🔴 **The strongest argument against: this makes `trusty-console` a
single point of failure for a security-critical path, and it is the component
least equipped to be one.** Today a webhook reaches `trusty-review` directly;
one process must be correct. After this, a delivery is correctly handled only
if console is running, its spool disk has space and is writable, its HMAC
check is right, its supervisor can spawn the target, the socket permissions
are actually applied, and the target's peer check works. Console becomes the
sole holder of the webhook secret and the sole verifier — a compromise of
console is now a compromise of every webhook-driven action across two
services, where previously the blast radius was one. The rejected
"webhook-only HTTP listener on each service" option, whatever else is wrong
with it, has none of these properties: no relay hop to drop a payload, no
supervisor to fail, no shared secret-holder, and each service's failure is
independent. This ADR accepts that centralisation because the ruling requires
it and because ADR-0011's original "one place to reason about external
exposure" intent argues for it — but the trade is real and it is not
security-neutral.

- **The spool is new persistent state on the ingress path**, with everything
  that implies: growth, corruption, and cleanup. A spool that silently stops
  being written reintroduces exactly the failure it was built to prevent, one
  level down.
- **Spawn latency is only negligible while the target starts fast.** It is
  currently 191 ms for `trusty-review`. For `trusty-analyze` it is not —
  see #5067 below.
- **Peer-credential checking is platform-specific** (`SO_PEERCRED` vs
  `getpeereid`) and is new code in the security path.
- Console must now be resident for webhooks to work at all. That is a weaker
  claim than it sounds — console is the HTTP surface, so it was already
  required — but it is now required for a path that previously did not
  involve it.

## Open Questions

- **Idle-timeout tuning for the supervisor** is left to implementation.
  `Bm25Supervisor` uses an LRU cap plus an RSS ceiling rather than an idle
  timer; a webhook target probably wants the timer. Not decided here.
- **Whether `trusty-analyze`'s webhook route survives at all.** Its
  `POST /webhooks/github` overlaps substantially with `trusty-review`'s, and
  #5028 measured zero deliveries to either. Retiring one is plausibly simpler
  than relaying to both, but that is a product question, not a transport one.

## Prerequisites

Two existing issues gate this work, and one of them is not currently labelled
as a criterion-(c) blocker:

- 🔴 **[#5067](https://github.com/bobmatnyc/trusty-tools/issues/5067) is a
  hard prerequisite for spawn-on-delivery against `trusty-analyze`**, not
  merely an independent performance defect. It records a measured 31m46s boot
  stall from an unconditional `NeuralEmbedder::new()` making an untimed
  hf-hub network call for a feature no caller uses. Spawn-on-delivery against
  a process that can take half an hour to bind its socket does not work at
  all. Under the current always-resident shape this cost is paid once per
  restart and is merely bad; under this ADR it is paid per delivery and is
  fatal.
- 🔴 **[#5064](https://github.com/bobmatnyc/trusty-tools/issues/5064) blocks
  criterion (c), not only the deferred criterion (d2).** #5064 is filed
  against the stdio/HTTP redb collision: `build_app_state` unconditionally
  opens `dedup.redb`, redb takes an exclusive cross-process flock, and the
  second opener degrades to `dedup: None` behind a swallowed `warn!`. That
  collision does not disappear when `trusty-review` stops being a resident
  daemon — it changes shape. A console-spawned webhook worker and a
  concurrent `serve --stdio` MCP session both call the same `build_app_state`
  against the same `--log-dir`, so the flock contention recurs between the
  spawned worker and the stdio session. Because the webhook path is the one
  that actually needs the dedup store (`allow_posting: true`, unlike the MCP
  path's hardcoded `false`), the degradation would land where it matters
  rather than where it is currently contained.

## Related Decisions

Vetted against `docs/adr/INDEX.md` and prior ADRs on 2026-08-07:

- **ADR-0032 (No service owns an HTTP daemon; console is the only HTTP
  surface):** **Amended.** ADR-0032's Decision is untouched and remains in
  force — no sibling daemon runs an HTTP listener, inter-service traffic is
  UDS, console is the only HTTP surface. This ADR closes the Open Question
  ADR-0032 explicitly declined to answer, and lifts the carve-out under which
  `trusty-analyze`'s and `trusty-review`'s webhook routes were allowed to
  remain. ADR-0032's Status becomes `Amended by 0034`; its
  Context/Decision/Consequences are not edited (DOC-46 §4).

- **ADR-0017 (Shared webhook ingress via console + Tailscale Funnel,
  Proposed):** **Extends, and diverges on one point.** ADR-0017's shared
  `/api/webhooks/{source}` endpoint terminating at console is adopted. It
  diverges in where the handler lives: ADR-0017 mounts the webhook handler in
  *trusty-mpm's* daemon API and has console reverse-proxy to it over HTTP,
  which ADR-0032 no longer permits. Under this ADR console terminates the
  request itself and relays over UDS. ADR-0017's "one verification seam"
  consequence survives intact and is in fact strengthened. ADR-0017's Funnel
  exposure mechanism is untouched by this ADR and remains Proposed.

- **ADR-0031 (Transport by purpose: UDS inter-crate, HTTP external,
  Proposed):** **Consistent — with a correction to a factual premise.** This
  ADR adopts ADR-0031's classification test and its JSON-RPC-over-UDS shape.
  It corrects one premise both ADR-0031 and ADR-0032 rely on: the `0600`
  socket permission that carries their access-control argument is not
  implemented anywhere in the workspace. This ADR makes it a requirement
  rather than continuing to cite it as an existing property.

- **ADR-0019 (Unified IPC messaging on the event bus):** **Consistent.**
  ADR-0019's durable IPC channel with explicit delivery acknowledgment is the
  same discipline the spool applies to webhook ingress. The spool is
  deliberately console-local and not built on the event bus, because it must
  be durable before console acknowledges GitHub — a narrower and earlier
  guarantee than bus delivery.

- **ADR-0011 (`tctl` owns service lifecycle; console owns the single HTTP
  surface):** **Consistent, with one boundary to watch.** ADR-0011 gives
  service lifecycle to `tctl`, and this ADR gives console the ability to
  spawn a target on demand. These do not conflict — `tctl` owns *supervised,
  long-lived* lifecycle; console owns an *ephemeral, request-scoped* child,
  the same split `trusty-memory` already has with `Bm25Supervisor` alongside
  launchd-managed daemons. The `TRUSTY_BM25_EXTERNAL`-style opt-out is
  carried forward so an operator running the target under `tctl` can tell
  console not to spawn it.

- **ADR-0018 (Loopback-only doctrine):** **Already superseded by 0032.**
  Noted only because its "Known Gaps" entry for `trusty-analyze`'s
  unproxied `/webhooks/github` is the gap this ADR finally closes.

- **ADR-0014 (Native MCP support) / ADR-0033 (`trusty-mcp` consolidation):**
  **Consistent.** Neither addresses webhook ingress. ADR-0033 notes
  `trusty-review` joins `trusty-mcp` once #5064 lands; this ADR raises #5064
  from a d2 prerequisite to a criterion-(c) prerequisite, which brings that
  dependency forward but does not change its direction.

No conflicts with any other Accepted ADR. Summary: amends ADR-0032 by
closing its Open Question, extends ADR-0017, corrects a factual premise
in ADR-0031, and leaves ADR-0011/0014/0019/0033 consistent.
