# Shared telemetry over UDS — research (2026-09-19)

Owner question: does it make sense to add a telemetry package — placed inside
`trusty-common`, as a new crate, or as an optional per-crate dependency — that
any workspace daemon can use, reachable over a Unix domain socket, to inspect
running crates for performance/memory optimisation and to write log files an
offline reader can consume. This report answers the seven questions in the
brief plus two owner follow-ups delivered mid-task: a fourth placement shape
(standalone optional-dependency crate) and a firm requirement that the
on/off switch work at **runtime**, on an **already-installed** binary, and be
operable by an **agent**, not only a human at a shell.

## 1. Inventory: what exists today

| Surface | Location | Scope |
|---|---|---|
| In-memory log ring + tracing layer | `crates/trusty-common/src/log_buffer.rs:39-42` (`LogBuffer`), `:155-170` (`LogBufferLayer`) | Per-daemon; installed by `init_tracing_with_buffer[_and_capture]` (`crates/trusty-common/src/tracing_init.rs:64-103,126-172`). Filter independent of stderr via `RUST_LOG_BUFFER` (`tracing_init.rs:92-95`), but read once at startup — not reloadable. |
| `logs_tail`-style dashboard consumption | trusty-search serves the same buffer to its UI (`crates/trusty-search/src/service/server/state_impl.rs:249-252`, `with_log_buffer`) | Per-daemon HTTP/UDS surface, no cross-daemon aggregation. |
| `console_metrics` MCP tool | One per daemon: `crates/trusty-analyze/src/mcp/console_metrics.rs`, plus `trusty-common/src/console_metrics/mod.rs:1-90` (shared contract: `ConsoleMetricsReport`, `ServiceHealth`, `CONSOLE_METRICS_METHOD = "console_metrics"`) | trusty-console polls each daemon's stdio MCP child every `poll_interval` (`crates/trusty-console/src/lib.rs:199`, `:370`, `:457`). Gated by trusty-common feature `console-metrics` (Cargo.toml:886). |
| Host/process metrics | `trusty-common/src/host_metrics/history.rs` (whole-machine CPU/mem/disk/net, feature `host-metrics`), `sys_metrics` (per-process, unconditional `system` sysinfo feature), `machine_tier` (RAM tier + budget, feature `machine-tier`, zero new deps — reads `sysctl`/`/proc`+cgroup directly, deliberately not sysinfo) | Already covers a chunk of "RSS/CPU for optimisation" — see §4. |
| Error capture | `crates/trusty-common/src/error_capture/store.rs`; consumed by `crates/trusty-mpm/src/daemon/bug_report/multi_store.rs`, `crates/trusty-mpm/src/daemon/state/core.rs`, `.../rpc/core_tests.rs`, `.../bin/tm/tracing_setup.rs` | trusty-mpm's `errors.jsonl` + `list_recent_errors`; per-daemon, JSONL is the existing convention for this kind of file output. |
| Log shipping (not telemetry) | `crates/trusty-common/src/log_drain/mod.rs:1-20` (feature `log-drain`) | Uploads existing daemon log files to `s3://`/`file://` for later diagnosis (#6533). Orthogonal: it drains logs that already exist; it does not decide what to log. |
| Control/event bus | `crates/trusty-common/src/control_bus/mod.rs:1-40` (feature `uds`, types-only + `push_client` gated `uds`+unix) | HarnessEvent/ActionEvent lifecycle taxonomy, owned end-to-end by trusty-console per the 2026-09-05 owner ruling ("the only event bus"). Built for workflow/session events, not perf histograms; reusing its envelope for telemetry would overload a taxonomy that already has an owner and a different purpose. |
| UDS transport primitives | `crates/trusty-common/src/uds/{rpc.rs,server/,peer.rs,sockbuf.rs,stream_client.rs,probe.rs}` | `send_framed_request[_capped]` (newline-JSON, one framing entry point, `rpc.rs:1-33`), same-UID peer check (`peer.rs:11-75`, `peer_uid_verdict`), socket buffer tuning (`sockbuf.rs`). This is the one existing UDS stack every daemon already uses or is migrating to (ADR-0031/0032). |
| MSRV/duplication guard | `docs/reference/common-pitfalls.md:10-18` | "Duplicating a shared capability instead of extending the common entry point" is a named anti-pattern — directly on point for telemetry. |

**Duplication found:** none rising to the level of the common-pitfalls rule —
`console_metrics` is already the one contract every daemon implements, and
`host_metrics`/`machine_tier`/`sys_metrics` are already the one place process/
machine numbers are read. The gap is depth (no latency histograms, no queue
depth, no span timing, no per-category runtime switch), not duplication.

**Issue #6572** (`gh issue view 6572`): titled "observability gaps:
classification cost, hook latency, and compression session id are not
recorded at info," milestone 1.6.5. Its three items are narrow logging-level
and field-population fixes inside `trusty-mpm` (promote existing debug logs
to info; add a latency line around hook invocations; populate `session_id` in
`compression.jsonl`). It proposes **no** shared package, no UDS surface, no
runtime switch — it is a trusty-mpm-local logging fix. This research **sits
beside #6572**, not inside it or replacing it: #6572's fixes are Rung 1-2 and
should ship on their own regardless of what this report recommends; nothing
here subsumes them, and nothing in #6572 anticipates a cross-crate telemetry
surface.

**Ports:** `docs/architecture/port-assignments.md:51-52` — `trusty-search`
(7878) and `trusty-mpm` (7880) are **still bound** as of this writing; #6285
and #6288 (both open) have not landed. The push toward UDS-only inspection
the brief describes is directionally correct per ADR-0031/0032 but not yet
complete.

**Prior owner rulings on record** (via `memory_recall`, palace `trusty-tools`):
console is "the only event bus" (2026-09-05); metrics should "push to
console — fold it into #6284" (2026-08-30) and non-daemon services push too,
console gains a pull API for third parties (2026-08-30 addendum); none of
these rulings have been implemented yet (metrics still flow console→poll, not
service→push). They are relevant context, not blocking constraints, since
they govern the console-aggregation path specifically and this report's
per-daemon inspection/file-output question is narrower.

## 2. Placement — four shapes, weighed on equal terms

**A. Plain module in `trusty-common`, always compiled in.** Rejected outright:
`trusty-common` has `default = []` and a `build.rs` zero-feature guard
(Cargo.toml:494, :500-502) specifically so nothing compiles by default; an
always-on module would be the one exception to a convention 47 features
already follow, and it is the heaviest-weight option for every consumer that
never wants it (nearly every crate in the workspace depends on
`trusty-common`, and it is published to crates.io under a semver gate —
`crates/trusty-common/Cargo.toml:1-98` documents live 0.x-break precedent).

**B. Feature-gated module in `trusty-common`** (e.g. `telemetry` feature,
following the `console-metrics`/`host-metrics`/`machine-tier` pattern at
Cargo.toml:886,891,897). Fits the crate's existing idiom exactly — this is
how `host_metrics`, `machine_tier`, and `console_metrics` itself were all
added, each documented with an explicit dependency-cost accounting in the
version-bump comment block. Cost when off: zero (not compiled). Semver
exposure: a new public module is a MINOR bump under the crate's own 0.x rule
(see the 0.44.0/0.48.0/0.49.0 precedent comments, Cargo.toml:10-12,67-71,
72-77) — routine, not a special risk, provided nothing existing changes shape.
Downside: it still centralises in the one crate every daemon depends on, so a
bug in the telemetry module's always-on core (see the runtime-switch
requirement below) is a bug in the dependency graph's most heavily-relied-on
node.

**C. New workspace crate, e.g. `trusty-telemetry`, taken as an ordinary
(non-optional) dependency by each daemon crate that wants it.** Isolates the
semver surface from `trusty-common` entirely — a crate nobody else depends on
can bump however it likes without touching the workspace's most-published
library. Costs one more crate to track in `cargo metadata`, one more
`Cargo.toml` in the 500-line doc-review rotation, and (if published) one more
crate to run through `preflight-publish.sh`. It does not have to be published
to crates.io at all — nothing requires every workspace crate to ship
externally, and a workspace-internal-only crate skips the semver gate
entirely, which is the strongest argument for this shape over B.

**D. (Owner addition) Small standalone crate, each consumer takes it as an
*optional* dependency behind a Cargo feature and/or a runtime switch, off by
default.** This is C's packaging with the activation question pushed down to
each consumer's own `Cargo.toml`. Concretely evaluated against the owner's six
criteria:

- **Compile-time/binary-size cost when off:** if "off" means the optional
  dependency is not even selected in the consumer's `Cargo.toml` feature set,
  cost is zero — same as B/C not being taken at all. But this collides with
  the owner's later, firmer requirement (see §3): the switch has to work on an
  *already-installed* `cargo install` binary, which means the core has to be
  compiled in unconditionally for every daemon that ships this. So "optional
  dependency, off by default" can only describe the **heavy** sub-parts
  (allocator profiling, `console-subscriber`), never the core — see the
  reconciliation at the end of this section.
- **Runtime overhead when off:** identical to B/C for the core (an atomic
  flag check), by construction, once the core is unconditionally compiled in.
- **Switching on an already-running daemon vs. requiring a restart/rebuild:**
  a Cargo feature can never do this — it is resolved at compile time. Only a
  runtime switch (env var re-read, or a live MCP/UDS toggle) can flip an
  already-running process. This is true regardless of which of B/C/D holds
  the code; it is a control-surface question, addressed in full in §3.
- **Can a released `cargo install` binary turn it on without a rebuild:**
  no, for anything gated behind a Cargo feature the released binary's
  manifest did not enable. Implication: diagnosing a problem on an already-
  installed host requires either (a) the always-on core already being
  compiled in and reachable by a runtime switch — the only path that needs no
  rebuild — or (b) `cargo install --features telemetry-heavy ...` and a
  daemon restart, which is a real but strictly slower path, appropriate only
  for the heavy, deliberately-not-always-on pieces.
- **Duplication across crates:** a shared crate (C or D) avoids re-writing
  the sink/atomic-flag/MCP-tool-pair logic per daemon exactly once each,
  which is what the common-pitfalls duplication rule (`common-pitfalls.md:
  10-18`) exists to prevent — this favors C/D over each daemon rolling its
  own, and is orthogonal to the B vs. C/D placement choice.
- **Semver exposure of `trusty-common`:** zero under C/D — the whole point of
  a separate crate is that its own version churns (which, given a new
  subsystem, will be frequent early on) never forces a `trusty-common` bump
  or touches the crate every other crate depends on.

**Reconciliation, not a fifth option:** the owner's D and the brief's B are not
actually in tension once the runtime-switch requirement (added mid-task) is
taken as given. The right shape is: **a small standalone crate (D's
packaging) whose lightweight core every consuming daemon enables
unconditionally (no "off" Cargo feature on the core — that would defeat the
no-rebuild requirement), with only the heavy, rebuild-requiring pieces kept
behind true off-by-default Cargo features.** This is recommended in §7 as the
one shape to build, ahead of B (plain trusty-common feature) specifically to
keep `trusty-common`'s semver surface and dependency weight untouched.

## 3. Control surface — how an agent switches this at runtime

The owner's firm requirement: an agent (not only a human at a shell) must be
able to turn telemetry on and off in an **already-running** daemon, at
runtime, with no rebuild.

**A Cargo feature is not a switch; it is a compile-time selection.** It is
ruled out as *the* mechanism, confirmed above. What has to be true instead:
the lightweight core (atomic per-category enable flags, the bounded JSONL
sink) is compiled into the released binary unconditionally, and *only* the
question of activation is a runtime concern.

**Today's reload story, checked, not assumed:** a workspace-wide grep for
`tracing_subscriber::reload` and `reload::Handle` returns zero matches — no
crate in this workspace reloads a tracing filter at runtime today.
`init_tracing_with_buffer` reads `RUST_LOG_BUFFER` once, at process start
(`tracing_init.rs:92-95`), and never again. **Per-category, per-level runtime
reload is new work**, not a wrapper around an existing mechanism — scope it
explicitly (see §7's first increment) rather than assuming
`tracing_subscriber::reload::Layer` is a drop-in; it is the right primitive
(a `Handle<EnvFilter, Registry>` swapped under an `RwLock`, standard
`tracing-subscriber` API since 0.2), but nobody has wired it up here yet.

**Three surfaces, one primary:**

1. **MCP tool pair, primary.** `telemetry_status` / `telemetry_set` added
   beside each daemon's existing `console_metrics` tool (same file pattern as
   `crates/trusty-analyze/src/mcp/console_metrics.rs`). This is the natural
   home: every daemon already exposes an MCP surface over the same hardened
   UDS socket (ADR-0031/0032), every agent in this family already reaches
   daemons through MCP, and it needs no new client code — an agent already
   has the tool-calling machinery. `telemetry_set` takes a category (latency,
   tokio/runtime, memory, spans), a level, and a duration/byte bound;
   `telemetry_status` reports what's on, since when, and how much budget is
   left.
2. **`tm`-style CLI verb, a wrapper, not a second implementation.** An agent
   that reaches a daemon through `Bash` rather than an MCP client (or a human
   at a shell) gets a verb — e.g. `tm daemon telemetry set search
   latency=debug --minutes 10` — that dials the same UDS socket and calls the
   same underlying handler `telemetry_set` calls. Per `common-pitfalls.md:
   10-18`, this must not become a parallel implementation: the CLI verb's
   Rust code should call the identical `trusty_telemetry::set(...)` entry
   point the MCP handler calls, both then serialising over the one
   `send_framed_request` framing (`uds/rpc.rs:1-33`).
3. **Raw UDS control message — the transport, not a fourth surface.** Both
   (1) and (2) are framed JSON-RPC over the same hardened UDS socket every
   daemon already listens on (`uds::server`, `uds::rpc::send_framed_request`).
   There is no separate "raw" wire protocol to design; "raw UDS" *is* what MCP
   and the CLI verb both already speak underneath. State that explicitly so a
   later reader does not go build a third framing.

**Signal categories toggle independently, each with its own level:**
`latency` (span timing around named hot paths), `runtime` (tokio task/queue
depth — see §4), `memory` (RSS/allocator samples), `spans` (raw span
enter/exit, the most expensive category — off by default even when
telemetry itself is on). Each is a small `AtomicU8` (level: off/info/debug)
checked with `Ordering::Relaxed` before any work happens on that code path —
the "near zero when off" bar the owner set. `tracing_subscriber::reload`
governs the *tracing-level* categories (latency spans, raw spans); the
memory/runtime samplers are plain polling loops gated by their own atomics,
not tracing filters, so reload is only part of the mechanism, not all of it.

**Safety of an agent-held switch:**

- **Auto-off bound.** Both a wall-clock duration (default e.g. 300s,
  caller-settable per `telemetry_set` call) and a byte cap on the JSONL sink;
  whichever fires first flips every category back off and is visible in the
  next `telemetry_status` call as the reason. This directly answers the
  "forgotten session fills the disk" risk.
- **State does not survive a daemon restart — recommended, not left open.**
  Telemetry always starts OFF on process start regardless of any prior
  on-disk state. A crash or an unrelated restart is a fail-safe: it can never
  silently resume writing after the operator/agent who turned it on is gone.
  The cost — an agent has to re-arm it after a restart it didn't expect — is
  strictly preferable to the alternative failure mode (state that outlives
  its own justification).
- **Who may switch it — no new trust boundary.** Identical to every other UDS
  control call in the family: the hardened socket's directory/mode plus the
  same-UID peer check (`peer_uid_verdict`, `uds/peer.rs:61`, used by every
  `uds::server` listener). Anyone who can already call `console_metrics` or
  any other MCP tool on that daemon can call `telemetry_set`; this is not a
  new privilege tier, and adding one would be scope the owner did not ask for.
- **The switch itself must never block or crash the daemon.** `telemetry_set`
  only ever flips atomics and, at most, opens a file handle for the sink;
  it does no I/O on the request-handling thread beyond that, and a sink-write
  failure (disk full, permission) degrades to "stop recording, keep serving"
  — never a panic, matching the crate-wide `no unwrap() in library code` rule
  (CLAUDE.md) and the Fail-Open posture the brief names.

## 4. Signals, and their overhead

| Signal | Source | Overhead when ON | Overhead when OFF | Default |
|---|---|---|---|---|
| Request/tool latency (span timing around named MCP handlers, embedder calls, index queries) | New `tracing::Instrument`-style spans + the reload-gated `latency` category | One `Instant::now()` pair + one JSONL line per call — cheap, this is exactly what explains the observed 8s search-query and 30s stdio-init cases | One atomic load | **On by default when telemetry is armed** — this is the headline signal the owner's pains call for |
| Queue depth / tokio task metrics | `tokio-metrics` `TaskMonitor` (stable API; some fields need `tokio_unstable`, see §6) or a manual poll of `tokio::runtime::Handle::metrics()` (stable, coarse) | Low-cost polling on a timer, not per-task instrumentation | none (poll loop simply not started) | Off in v1; needs `tokio_unstable` for the richest fields — see §6 caveat |
| RSS / allocator stats | Reuse `crates/trusty-common/src/host_metrics` (per-machine) and the unconditional `sys_metrics` (per-process, `sysinfo` "system" feature already always-on) — no new dependency | A `sysinfo::System::refresh_process` call is not free (syscalls) but is already paid elsewhere in the codebase at low frequency; sampling every few seconds is affordable | none if sampler not started | On — this already exists, wiring it into the JSONL sink is nearly free |
| Allocator-level heap profiling (per-allocation) | `dhat` (needs to *be* the global allocator) | High — every allocation instrumented; only viable as a special debug build | n/a (compiled out entirely unless the special build is used) | Off, heavy/rebuild-only tier |
| Embedder/ONNX timings | Wrap existing `embedder_client`/`embedder` call sites with the same `latency` span category | Same as request/tool latency | Same | On by default when armed |
| Index sizes / fd counts | fd count: `/proc/self/fd` (Linux) or `libproc`/`getrlimit`+enumeration (macOS) — new, small; index sizes are already reported by each daemon's own `console_metrics` payload | Cheap, infrequent poll | none | On, low frequency |
| Cold-start spans | Wrap daemon `main()`/init sequence in the same span category, written once per process lifetime | Negligible — one-shot | n/a | On |

**No signal above requires jemalloc or a custom allocator.** The workspace
sets no global allocator today (checked: no `#[global_allocator]`,
`jemalloc`, or `mimalloc` anywhere in `crates/*/src/main.rs`,
`crates/*/src/bin/*.rs`, or any `Cargo.toml`). `sys_metrics`'s RSS number
comes from the OS (`sysinfo`), not the allocator, so RSS tracking needs no
allocator change at all. Switching to jemalloc/mimalloc purely to get
per-allocation-class stats (what `dhat` or `jemalloc`'s own stats API would
add beyond RSS) is a materially bigger, workspace-wide decision — a new
process-global dependency for every binary, platform-specific build
concerns — and should stay a deferred, separately-decided item, not bundled
into this telemetry package.

## 5. File output and reader

**JSONL, not a bespoke format, and not Chrome-trace/Perfetto as the default
path.** Rationale, concretely tied to the owner's stated agent loop (arm,
reproduce, disarm, read, reason):

- The repository already has two JSONL conventions doing exactly this job at
  smaller scope — `errors.jsonl` (`crates/trusty-mpm/src/daemon/bug_report/
  multi_store.rs`, `list_recent_errors`) and `compression.jsonl`
  (`crates/trusty-mpm/src/bin/tm/commands/compress.rs`, per issue #6572's
  own finding about its missing `session_id`). A telemetry JSONL file is the
  same shape again, not a new convention to learn.
- **Token cost is the deciding factor for the agent loop specifically.** A
  50 MB Chrome trace (`tracing-chrome`) or a `tracing-flame` flamegraph is
  built to be opened in a GUI viewer (`chrome://tracing`, Perfetto UI,
  `inferno`) — exactly the tool an agent does not have and should not be
  asked to parse byte-for-byte. JSONL, one line per span-close event, is
  `grep`/`jq`-able and can be tailed to the last N lines cheaply.
- **A bounded summary MCP call answers the owner's "should the daemon offer
  this" question: yes.** Add `telemetry_summary` beside `telemetry_status`
  returning top-N slow spans, a small histogram (bucketed p50/p95/p99 per
  category), and the RSS delta over the capture window, computed server-side
  from the same in-memory counters that also feed the JSONL sink. This means
  the default agent loop never has to open the raw file at all — it arms,
  reproduces, disarms, calls `telemetry_summary`, and reasons from a payload
  sized like `console_metrics` already is. The raw JSONL stays on disk for a
  human, for trusty-console, or for a deeper agent dive that names a
  specific span.
- `tracing-chrome`/`tracing-flame` remain available as an **optional export
  path** (convert the JSONL to Chrome-trace format on demand, or add them as
  an additional `Layer` under the heavy/rebuild-gated feature tier) for a
  human debugging with an existing viewer — never the default write target.

**Rotation, size bounds, location:** follow the existing data-dir and
log-rotation conventions rather than inventing new ones. #8270 (`com.trusty.
search.logrotate runs newsyslog as non-root and fails every run`, currently
`status:in-progress`) is exactly the sharp edge to avoid repeating: whatever
rotates the telemetry JSONL should not assume root, and should reuse
whichever mechanism #8270 lands on for trusty-search's own log rotation
rather than adding a second one. Size bound is already covered by the
auto-off byte cap in §3 — the file cannot grow past that cap while a given
capture session is live; rotation/retention across multiple capture
sessions over time is the open item #8270's fix should be checked against
before this ships.

**Is a bespoke reader needed?** No. `trusty-console` already knows how to
poll a daemon's MCP surface and render a Svelte panel from a JSON payload
(the `console_metrics` pattern) — `telemetry_summary`'s payload slots into
that same rendering path with no new console-side protocol. A raw-JSONL
viewer, if ever wanted, is `jq`/`grep`/a spreadsheet import, not new code.

## 6. Build vs. adopt

| Crate | Fit | Dependency/compile cost | MSRV 1.94 | Licence | Maintenance (2026) |
|---|---|---|---|---|---|
| `tracing` + `tracing-subscriber` | Already the workspace standard; `reload::Layer`/`reload::Handle` is the missing piece, not a new dependency | Zero new — both are already unconditional deps of `trusty-common` | Yes, already in use | MIT | tokio-rs, actively maintained — [github.com/tokio-rs/tracing](https://github.com/tokio-rs/tracing) |
| `metrics` + `metrics-exporter-*` | Poor fit as delivered — its exporters (Prometheus, StatsD) assume an HTTP scrape endpoint or a UDP sink, neither of which matches "console is the only HTTP surface" (ADR-0032) or "push over UDS"; would need a hand-written UDS exporter, which is most of the work this report proposes anyway | New dependency tree, moderate | Should be fine (pure Rust) | MIT | metrics-rs org, maintained — [github.com/metrics-rs/metrics](https://github.com/metrics-rs/metrics) |
| `opentelemetry` + OTLP | Overkill for a single-host, no-collector environment; the workspace runs no Jaeger/Tempo/OTLP collector today, so OTLP export has nowhere to land without standing up new infrastructure | Large — `tonic`/`prost`/gRPC stack, materially increases compile time and binary size | Should be fine | Apache-2.0 | open-telemetry org, actively maintained but heavy — [github.com/open-telemetry/opentelemetry-rust](https://github.com/open-telemetry/opentelemetry-rust) |
| `tokio-console` / `console-subscriber` | Good fit for the "tokio task/queue depth" signal specifically, but requires the `tokio_unstable` cfg flag workspace-wide for the crate(s) that enable it — this is not a per-crate feature flag, it changes the ABI contract tokio itself is built against, so it belongs in the heavy/rebuild-only tier, opt-in per debug build, never in the default install | New dependency, `tokio_unstable` build-flag requirement is the real cost | Should be fine, flag is the concern not MSRV | MIT | tokio-rs, actively maintained — [github.com/tokio-rs/console](https://github.com/tokio-rs/console) |
| `tokio-metrics` | Partial fit: the basic `TaskMonitor` API works on stable tokio; the richer per-task poll/idle histograms need the same `tokio_unstable` flag as `console-subscriber` — verify which fields the workspace actually wants before committing | Small dependency; cost is the same unstable-flag concern | Should be fine | MIT | tokio-rs — [github.com/tokio-rs/tokio-metrics](https://github.com/tokio-rs/tokio-metrics) |
| `dhat` (`dhat-rs`) | Fits heap-profiling specifically, but only as a special debug binary — it must be the process's global allocator, which this workspace does not set today (verified: no `#[global_allocator]` anywhere in-tree) | Small dependency, but "cost of switching allocators" is really "cost of adopting a process-global allocator for the first time," a bigger decision than telemetry itself | Should be fine | Apache-2.0/MIT | maintained, used by `dhat-heap` examples across the ecosystem — [github.com/nnethercote/dhat-rs](https://github.com/nnethercote/dhat-rs) |
| `pprof-rs` | CPU sampling profiler; Linux support is solid, macOS support exists but is the less-travelled path for this crate (signal-based sampling, historically the flakier platform for it) — a real portability risk given this workspace is primarily developed on macOS | Small-moderate dependency | Should be fine | MIT | maintained, moderate activity — [github.com/tikv/pprof-rs](https://github.com/tikv/pprof-rs) |

**Verdict:** build the small always-on core on top of `tracing`/
`tracing-subscriber` (already present, zero new cost) plus the existing
`sys_metrics`/`host_metrics` process/machine readers (already present, zero
new cost). Treat `console-subscriber`/`tokio-metrics`'s unstable-flag
features, `dhat`, and `pprof-rs` as the heavy, feature-gated, rebuild-only
tier the owner's item 1 explicitly allows to stay gated. Do not adopt
`metrics` or `opentelemetry` — both solve a problem (external scrape/export)
this workspace does not have yet, and both would require building the same
UDS-transport glue this report proposes writing directly.

## 7. Recommendation

**Yes, this makes sense** — the owner's three named pains (a 199-minute
install at load average 60, a 30s cold trusty-analyze stdio-init timeout
[#8279], an 8s trusty-search query on a busy host) are all latency
questions that no signal in the codebase today can answer after the fact;
`console_metrics`/`host_metrics` report point-in-time gauges, not "which
call took 8 seconds and why."

**One recommended shape:** a small standalone crate, `trusty-telemetry`
(placement **D**, reconciled per §2) — not published to crates.io initially,
so it carries no semver gate. Its lightweight core (per-category atomic
enable flags, `tracing_subscriber::reload`-based level control for the
tracing-backed categories, a bounded JSONL sink with the duration+byte
auto-off from §3) is a **mandatory, always-compiled** dependency for any
daemon that adopts it — never an off-by-default Cargo feature on the core,
because that would silently reintroduce "needs a rebuild" for the exact case
the owner ruled out. Heavy pieces (`console-subscriber`, `dhat`, `pprof-rs`)
live behind true off-by-default features on the same crate, each requiring a
deliberate special build, matching the owner's own carve-out. Control surface
is the MCP tool pair `telemetry_set`/`telemetry_status`/`telemetry_summary`
beside each daemon's existing `console_metrics` tool, with a `tm` CLI verb as
a thin wrapper over the identical entry point — never a third
implementation — both riding the same hardened UDS socket and same-UID peer
check every other MCP call on that daemon already relies on.

**First increment, sized for one PR:** land `trusty-telemetry` with (a) the
atomic per-category flags and the reload-based level control for exactly one
category (`latency`), (b) the bounded JSONL sink with its auto-off timer and
byte cap, (c) `telemetry_status`/`telemetry_set`/`telemetry_summary` wired
into **one** pilot daemon — `trusty-search`, since it already has the
observed 8s-query pain to validate against — and (d) span timing added to
that daemon's query-execution and cold-start paths only. This is a
**cross-crate change touching a shared library and one daemon's public MCP
surface**, so it maps to **Rung 4** on the repository's test ladder
(`CLAUDE.md`'s Rust Test Ladder table) at minimum: Rung 3 on
`trusty-telemetry` itself, then `SKIP_UI_BUILD=1 check --workspace` plus
`test -p trusty-search --no-fail-fast` for the one direct dependent. Given
the new process-lifecycle surface (a background sink, an auto-off timer),
lean toward Rung 5's failure-path coverage for the auto-off/bound logic
specifically, even though the change is scoped to one daemon.

**Defer, explicitly, out of the first PR:** the `runtime`/tokio-queue-depth
and `memory`/allocator-sampler categories (ship `latency` alone first, add
the others once the reload plumbing is proven); rollout to any daemon beyond
trusty-search; the heavy rebuild-only tier (`console-subscriber`, `dhat`,
`pprof-rs`) entirely; any Chrome-trace/Perfetto export path; any persistence
of telemetry state across a daemon restart (recommended never, per §3, not
merely deferred); any trusty-console UI beyond consuming
`telemetry_summary`'s payload the same way it already consumes
`console_metrics`'s; and the fd-count / index-size signals in §4's table,
which are cheap but not needed to validate the pilot.

**Strongest argument against doing it at all:** every one of the owner's
three named pains is, in principle, answerable today by adding two or three
targeted `tracing::info!`/span-timing lines at the specific call sites
already suspected (the install path, the stdio `initialize` handler, the
search query path) and reading them through the `LogBuffer`/`RUST_LOG_BUFFER`
mechanism that already exists, with zero new crate, zero new control
surface, and zero new trust-boundary-adjacent code to secure. Nobody has
tried that narrower fix and found it insufficient — this report was not
asked to, and did not, run that experiment first. Building a whole always-on
subsystem (atomics on every request path, a sink, an auto-off timer, an MCP
tool triplet) adds nonzero surface area to every daemon that adopts it, and
a bug in the safety net itself (the auto-off timer failing to fire, the sink
write blocking under disk pressure) is a new failure mode that plain,
ad-hoc tracing spans structurally cannot have. If the two- or three-line fix
turns out to answer the three named pains, the standing subsystem this
report recommends is solving a problem that does not yet exist.

## Open questions for an owner decision

1. Publish `trusty-telemetry` to crates.io eventually, or keep it
   workspace-internal indefinitely? Keeping it internal avoids the semver
   gate entirely but means no external consumer can use it standalone.
2. Confirm the pilot daemon: this report assumes `trusty-search` (it has the
   concretely observed 8s-query pain); trusty-analyze's #8279 stdio-init
   timeout is an equally strong candidate and may be a better first target if
   its root cause needs span-level timing more urgently than search's does.
3. Confirm the recommended "never persist telemetry state across a restart"
   default — an owner who wants a long unattended capture across a scheduled
   restart would need a different answer than the fail-safe one recommended
   here.
4. Confirm the auto-off defaults (300s / a specific byte cap) — this report
   picked illustrative numbers, not owner-specified ones.
5. Whether `#6572`'s three fixes should land first, in parallel, or wait —
   this report recommends first/parallel (they are Rung 1-2 and unrelated to
   the new crate), but sequencing is the owner's/PM's call.
