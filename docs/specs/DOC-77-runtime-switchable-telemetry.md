# DOC-77 — Runtime-Switchable Telemetry for Trusty Daemons

**Status:** Draft
**Spec ID:** `SPEC-TELEM-01~draft` … `SPEC-TELEM-10~draft` (DOC-77)
**Subsystem:** new workspace crate `trusty-telemetry` (unpublished); first
consumer `trusty-search`; later phases add `trusty-memory`, `trusty-mpm`,
`trusty-analyze`, `trusty-embedderd`, `trusty-console`
**Owner:** Engineering (trusty-common / trusty-search)
**Last-updated:** 2026-09-19
**DOC-N claim:** `DOC-77`, scan-before-claim per
[DOC-38 §4.1](./spec-linked-documentation.md#SPEC-SLD-01~draft). Verified
free: `docs/specs/README.md`'s catalog note ("Next free `DOC-N` = `DOC-77`",
recorded 2026-09-16) is current on `origin/main` — the only `DOC-77` mention
under `docs/specs/**` was that note itself
(`docs/specs/README.md:107-108`), no open pull request names `DOC-77`
(`gh pr list --search "DOC-77" --state open`, checked 2026-09-19), and no
self-labeled spec header elsewhere claims it.
**Builds on:** the research report
[`docs/research/shared-telemetry-over-uds-2026-09-19.md`](../research/shared-telemetry-over-uds-2026-09-19.md)
(§2, §3, §5-7), commissioned by and answering to the owner's questions
recorded there.
**Epic:** [#8295](https://github.com/bobmatnyc/trusty-tools/issues/8295)
**Cross-ref:** Phase 1 issue
[#8296](https://github.com/bobmatnyc/trusty-tools/issues/8296). Independent
of [#6572](https://github.com/bobmatnyc/trusty-tools/issues/6572) (narrower,
trusty-mpm-local logging fixes; this spec does not subsume it).

---

## 1. Scope and non-goals {#SPEC-TELEM-01~draft}

This spec covers a new workspace crate, `trusty-telemetry`. It is not
published to crates.io. Each adopting daemon (`trusty-search` first) takes it
as an ordinary, mandatory dependency: its lightweight core is always compiled
in. **A Cargo feature is never the on/off switch** — the switch is a runtime
control reachable while the daemon is already running, on an already-`cargo
install`-ed binary, with no rebuild.

Heavy diagnostic tools — `console-subscriber`, `tokio-metrics`'s
`tokio_unstable`-gated fields, `dhat`, `pprof-rs` — live behind true
off-by-default Cargo features on the same crate. Each needs a deliberate
special build. They are out of phase 1 (§9).

This spec does not adopt the `metrics` crate or `opentelemetry`. `metrics`'s
exporters assume an HTTP-scrape or UDP sink, neither of which matches this
workspace's UDS-only control-plane posture; `opentelemetry`/OTLP needs a
collector (Jaeger, Tempo) this workspace does not run. Detail:
[research report §6](../research/shared-telemetry-over-uds-2026-09-19.md#6-build-vs-adopt).

This work is independent of #6572: that issue promotes three existing
`trusty-mpm` debug logs to info and adds one latency line around hook
invocation, entirely inside `trusty-mpm`, with no shared crate and no runtime
switch. It should land on its own schedule regardless of this spec.

## 2. Category model {#SPEC-TELEM-02~draft}

Three independently-switchable categories: `latency` (span timing around
named hot paths — MCP handlers, embedder calls, index queries), `memory`
(RSS/process samples from the existing `sys_metrics`/`host_metrics` readers),
`runtime` (tokio task/queue depth). Each category carries its own level:
`off` | `info` | `debug`. On the `latency` category, `debug` additionally
enables raw span enter/exit tracing — the research report's separate
`spans` signal, folded here into a level rather than a fourth category, to
match this spec's three-category directive.

Each category's enable state is one `AtomicU8` (the level), read with
`Ordering::Relaxed` before any work happens on that code path. Off-state cost
target: **one atomic load, no allocation, per instrumented call site.**

`tracing_subscriber::reload` (`reload::Handle<EnvFilter, Registry>` behind an
`RwLock`, standard API since `tracing-subscriber` 0.2) governs the
tracing-backed category (`latency`). A workspace-wide grep for
`tracing_subscriber::reload`/`reload::Handle` returns zero matches today
(verified during the research pass) — this reload wiring is new work, not a
wrapper around an existing mechanism. `memory` and `runtime` are plain
polling loops gated by their own atomics, not tracing filters; reload is part
of the mechanism, not all of it.

## 3. Control surface — MCP tool contracts {#SPEC-TELEM-03~draft}

Three MCP tools, added beside each daemon's existing `console_metrics` tool
(same file pattern as `crates/trusty-analyze/src/mcp/console_metrics.rs`),
reachable only over the daemon's existing hardened UDS socket. A `tm` CLI
verb is a thin wrapper calling the identical entry point these three tools
call — never a second implementation, per the common-pitfalls duplication
rule (`docs/reference/common-pitfalls.md:10-18`).

### `telemetry_set`

- **Params:** `category` (string enum: `latency` | `runtime` | `memory`,
  required), `level` (string enum: `off` | `info` | `debug`, required),
  `duration_secs` (u64, optional, default `300`, hard max `3600`),
  `max_bytes` (u64, optional, default `52428800` [50 MiB], hard max
  `524288000` [500 MiB]).
- **Result:** `category`, `level` (the level now in effect), `armed_until`
  (RFC 3339 timestamp, absent when `level == "off"`), `byte_cap` (the bound in
  effect), `bytes_used_so_far` (u64), `previous_level`, `clamped` (bool — true
  if a supplied `duration_secs`/`max_bytes` exceeded its hard max and was
  capped to it).
- **Errors:** the sink cannot be opened (disk full, permission denied, path
  missing) — returned as a typed error result, never a panic; category stays
  at its previous level. A reload failure on the `latency` category — returned
  the same way, level unchanged.

### `telemetry_status`

- **Params:** none.
- **Result:** one entry per category — `name`, `level`, `armed_since`,
  `armed_until`, `bytes_used`, `byte_cap`, `last_auto_off_reason`
  (`"duration"` | `"bytes"` | `null`) — plus `sink_healthy` (bool) and
  `last_error` (string, optional).

### `telemetry_summary`

- **Params:** `category` (optional filter), `top_n` (u32, optional, default
  `20`, hard max `100`).
- **Result:** `window_start`, `window_end` (RFC 3339), `spans` — array of
  `{name, count, p50_ms, p95_ms, p99_ms}` sorted slowest-first, `rss_delta_bytes`
  (i64, may be negative), `byte_ceiling` (the serialized-response cap, matching
  `console_metrics`'s own bounding), `truncated` (bool — true if more spans
  exist than `top_n` returned).

**Trust boundary.** Identical to every other UDS control call in the family:
the socket directory's `0700` mode, the socket file's `0600` mode, and the
same-UID peer check (`peer_uid_verdict`,
`crates/trusty-common/src/uds/peer.rs:61-68`) that every `uds::server`
listener already applies. Anyone who can already call `console_metrics` or
any other MCP tool on that daemon can call these three; this adds no new
privilege tier. All three ride the same newline-framed JSON transport every
other UDS control call uses (`send_framed_request`,
`crates/trusty-common/src/uds/rpc.rs:1-14`) — there is no separate wire
protocol to design.

## 4. Safety bounds — auto-off {#SPEC-TELEM-04~draft}

Two independent auto-off bounds, whichever fires first flips every category
back to `off`:

- **Duration.** Default `300` seconds (the research report's own illustrative
  figure, §3). A caller may set any value from `1` to a hard maximum of
  `3600` seconds (one hour); a request above the maximum is clamped to it
  (`telemetry_set`'s `clamped` field reports this).
- **Bytes.** Default `52428800` bytes (50 MiB) on the JSONL sink. A caller
  may set any value up to a hard maximum of `524288000` bytes (500 MiB);
  same clamp-and-report behavior.

The reason the bound fired is visible in the next `telemetry_status` call
(`last_auto_off_reason`). This directly answers the "an agent forgets to
disarm it and fills the disk" risk.

**State never persists across a daemon restart.** A daemon always starts
with every category at `off`, regardless of what was armed before the
restart. A crash or an unrelated restart is a fail-safe, not a loss: it can
never silently resume writing after the agent or operator who armed it is
gone. The cost is that an agent must re-arm after an unexpected restart —
preferable to state that outlives its own justification.

## 5. Failure behavior (Fail-Open) {#SPEC-TELEM-05~draft}

Four failure modes, each required to leave the daemon serving, panic-free,
and non-blocking on the request path; each visible in `telemetry_status`.

- **Sink failure (disk full, permission denied, path missing).** At
  `telemetry_set` time: the open fails, the call returns a typed error, the
  category stays `off`. Mid-capture (disk fills after some data is written):
  writes stop, the category's `bytes_used` freezes, `sink_healthy` flips to
  `false`, `last_error` carries the OS error text. Serving continues
  unaffected — matching the crate-wide "no `unwrap()` in library code" rule
  (`CLAUDE.md`).
- **A slow disk.** The sink writer never runs on the request-handling thread.
  Span-close events are pushed onto a bounded channel; a background writer
  drains it to the JSONL file. If the writer falls behind and the channel
  fills, new lines are dropped (never blocked on), and a saturating
  `dropped_lines` counter is incremented — surfaced in `telemetry_status` as
  part of the category's byte-usage detail.
- **A full in-memory buffer.** The `telemetry_summary` accumulator (the
  per-span-name histogram feeding p50/p95/p99) is a fixed-capacity structure,
  not an unbounded map: once at capacity, the least-frequently-seen span name
  is evicted to admit a new one. It never grows past its configured cap
  regardless of capture duration.
- **A reload failure.** `tracing_subscriber::reload::Handle::reload()`
  returning `Err` (rare — an internal poisoned-state case) leaves the
  category at its previous level; `telemetry_set` returns a typed error. The
  change is never partially applied.

No mode above panics, blocks a request, or requires operator intervention to
recover serving; only the affected category's own future accuracy degrades.

## 6. Output — JSONL schema and reader {#SPEC-TELEM-06~draft}

JSONL, one line per span-close event, matching the workspace's two existing
JSONL conventions at smaller scope (`errors.jsonl`,
`crates/trusty-mpm/src/daemon/bug_report/multi_store.rs:45-48`;
`compression.jsonl`,
`crates/trusty-mpm/src/bin/tm/commands/compress.rs`) — not a new format to
learn, and not Chrome-trace/Perfetto by default (those stay an optional,
later export path for a human with an existing viewer).

**Schema, one line per event:**

```json
{"ts":"2026-09-19T14:32:07.512Z","category":"latency","span":"search.query","duration_ms":8123.4,"request_id":"a1b2c3d4","daemon":"trusty-search"}
```

| Field | Type | Meaning |
|---|---|---|
| `ts` | string (RFC 3339, millisecond precision, UTC) | When the span closed |
| `category` | string enum | `latency` \| `runtime` \| `memory` |
| `span` | string | Span/call-site name (e.g. `search.query`, `embedder.embed`) |
| `duration_ms` | f64 | Wall-clock duration in milliseconds |
| `request_id` | string | Opaque per-request correlation id |
| `daemon` | string | Which daemon wrote the line (`trusty-search`, …) — carried from phase 1 so a later cross-daemon reader needs no schema change |

**Location.** Follows the existing data-dir convention:
`resolve_data_dir("trusty-search")` (`crates/trusty-common/src/data_dir.rs:129`)
joined with a `telemetry/` subdirectory, mirroring how `errors.jsonl` sits
under `<data_dir>/<app_name>/`. No root needed — see #8270 below. Naming is
per capture session: `telemetry-<category>-<armed-at-RFC3339-compact>.jsonl`
(e.g. `telemetry-latency-20260919T143207Z.jsonl`), so an agent's later
`telemetry_summary` call and the raw file from the same arm/disarm cycle are
trivially paired by timestamp.

**Rotation.** [#8270](https://github.com/bobmatnyc/trusty-tools/issues/8270)
(`com.trusty.search.logrotate runs newsyslog as non-root and fails every
run`, currently `status:in-progress`) is the sharp edge to avoid repeating:
whatever prunes old telemetry files must not assume root, and should reuse
whatever mechanism #8270 lands on rather than adding a second rotation path.
The byte cap in §4 already bounds a single capture session's file; retention
across multiple sessions (how many old files to keep) is open — §10.

**No bespoke reader is needed.** `telemetry_summary` (§3) answers the "should
the daemon offer a reader" question directly: an agent's default loop never
opens the raw file. `trusty-console` already knows how to poll a daemon's MCP
surface and render a payload (the `console_metrics` pattern); `telemetry_summary`
slots into that same rendering path, deferred to phase 4 (§9). A raw-JSONL
viewer, if ever wanted, is `jq`/`grep`, not new code.

## 7. The agent loop {#SPEC-TELEM-07~draft}

Arm, reproduce, disarm, summarize — four calls, no raw file read:

```
telemetry_set(category="latency", level="debug", duration_secs=60)
  -> {level: "debug", armed_until: "...T14:33:07Z", byte_cap: 52428800, ...}
# reproduce the slow query out-of-band
telemetry_set(category="latency", level="off")
  -> {level: "off", previous_level: "debug", ...}
telemetry_summary(category="latency", top_n=10)
  -> {spans: [{name: "search.query", count: 1, p50_ms: 8123.4, ...}], rss_delta_bytes: 4194304, ...}
```

The agent reasons from the fourth call's bounded payload — sized like
`console_metrics` already is — without ever touching the JSONL file on disk.

## 8. CPU profiling: a `samply` trial in the heavy tier {#SPEC-TELEM-08~draft}

Verified facts, each cited to its source (checked 2026-09-19):

- **License.** `MIT OR Apache-2.0` (dual, user's choice) —
  [crates.io/crates/samply](https://crates.io/crates/samply) (crate metadata,
  version 0.13.1) and
  [github.com/mstange/samply — README, License section](https://github.com/mstange/samply/blob/main/README.md#L139-L146).
- **Shape.** A command-line binary, not a library to link: the crate publishes
  `bin_names: ["samply"]` and `has_lib: false`
  ([crates.io/crates/samply](https://crates.io/crates/samply)). It runs as a
  separate process.
- **Platform support.** macOS and Linux (also Windows) —
  ["samply works on macOS, Linux, and Windows."](https://github.com/mstange/samply/blob/main/README.md#L5)
- **Attach by pid.** `samply record -p <pid>` / `--pid <pid>` attaches to an
  already-running process by pid instead of spawning a new one —
  [`samply/src/cli.rs:174-176`](https://github.com/mstange/samply/blob/main/samply/src/cli.rs#L174-L176)
  (`RecordArgs.pid: Option<u32>`, "Process ID of existing process to attach
  to.").
- **macOS attach requirement.** A one-time `samply setup` step (re-run after
  every `samply` update) self-signs the samply binary; this is required
  specifically to attach to an already-running process on macOS —
  [README, "Known issues"](https://github.com/mstange/samply/blob/main/README.md#L137)
  and
  [`samply/src/cli.rs:66-68`](https://github.com/mstange/samply/blob/main/samply/src/cli.rs#L66-L68)
  (`Action::Setup`, "Codesign the samply binary on macOS to allow attaching to
  processes."). The same README section notes samply cannot profile
  system-signed macOS binaries, but can profile anything self-built or
  installed by `cargo install` or Homebrew — which covers this workspace's
  `cargo install`-only daemons (`CLAUDE.md`'s macOS rule).
- **Output format.** The Firefox Profiler's own JSON schema (the
  `fxprof-processed-profile` crate in the same repository), opened at
  [profiler.firefox.com](https://profiler.firefox.com/); the default output
  file is gzip-compressed, `profile.jslb.gz`
  ([`samply/src/cli.rs:156`](https://github.com/mstange/samply/blob/main/samply/src/cli.rs#L156)).

**Where it sits.** `samply` is the CPU-profiling option in the heavy,
rebuild-only tier, evaluated against `pprof-rs` (§6 of the research report).
The report rates `pprof-rs`'s macOS support as its weaker platform — this
workspace is developed mainly on macOS, and `pprof-rs`'s signal-based
sampling is the less-travelled path there. `samply`'s macOS support is native
to the tool (its own primary use case), so **the trial favors `samply` over
`pprof-rs` on paper for this workspace.** Because `samply` is an external
process attaching by pid — no code links against it, no `tokio_unstable` flag,
no allocator change — it needs **no rebuild of the daemon to produce a
profile**. This moves CPU profiling out of the rebuild-only tier for the act
of capturing a profile; it stays a manually-invoked heavy tool, never part of
the always-on core.

**Trial requirement `REQ-SAMPLY-1` (testable).** On a macOS host, with
`trusty-search` already running as a `cargo install`-ed binary and a one-time
`samply setup` already done: run `samply record -p <trusty-search pid>
--duration <bounded seconds> --save-only -o <path>.jslb.gz` while reproducing
a query already known to take roughly 8 seconds. **Pass** requires all three:
(a) `samply` produces a non-empty profile file at the given path; (b)
`trusty-search` continues to answer requests of the same query type during
capture; (c) that query's wall-clock latency during capture stays within a
stated tolerance (no more than 2x its uncaptured latency) of its latency
without capture. **Fail** is any of the three not holding, `samply` exiting
non-zero, or the binary missing/`samply setup` failing. A failed trial drops
`samply` from the plan; it does not block phase 1 (#8296), which does not
depend on this trial.

**Relation to the crate.** `trusty-telemetry` does not link `samply`. If the
trial passes, a later phase may add a `telemetry_profile` control that spawns
`samply record -p <own pid> ...` against the daemon's own process, with the
same duration/byte bounds and the same never-block, never-crash failure rules
as the JSONL sink (§5). A missing `samply` binary on `PATH` is a reported
error in `telemetry_status`/`telemetry_profile`'s result, never a panic.

**The agent-loop problem this trial does not solve.** A Firefox Profiler
JSON file is large — unlike `telemetry_summary`'s bounded payload, nothing
today produces a bounded top-N hot-function digest from it that an agent
could read instead of opening the raw file. No existing tool in this
workspace does that conversion. This is left as an open question (§10), not
solved by the trial itself.

**Phase placement.** The trial is its own phase item, not folded into
phase 1's acceptance criteria (§9) — it needs no `trusty-telemetry` code, only
an already-running `trusty-search` binary, so it can run independently of the
crate's own schedule. It is placed after phase 1 lands (§9, "Phase 1.5") so
the trial's reproduction step has a known, already-instrumented slow query to
correlate against, and a stable pilot binary to attach to.

## 9. Phase plan and test ladder {#SPEC-TELEM-09~draft}

**Phase 1 — #8296.** `trusty-telemetry` core, `latency` category only,
`trusty-search` pilot. Acceptance, restated as testable requirements:

| ID | Requirement | Test kind |
|---|---|---|
| REQ-1 | Telemetry off: one atomic load, zero allocation, per instrumented call site | benchmark |
| REQ-2 | `telemetry_set` toggles `latency` on/off in a running `trusty-search` with no restart; `telemetry_status` reports state, time, and bytes remaining; both reachable only over the existing UDS MCP surface | integration |
| REQ-3 | Auto-off fires at the duration bound and at the byte bound, whichever comes first | integration (timing) |
| REQ-4 | A sink failure (disk full, permission denied, path missing) stops recording and keeps serving — no panic, no blocked request, visible in `telemetry_status` | failure-path |
| REQ-5 | Telemetry is `off` after a daemon restart, regardless of prior state | integration |
| REQ-6 | `telemetry_summary` returns top-N slow spans, p50/p95/p99 per span name, and RSS delta over the capture window, bounded in bytes; an agent completes arm → reproduce → disarm → summary without opening the raw file | integration |
| REQ-7 | JSONL lines carry span name, duration, timestamp, and a per-request identifier; the file lives under the daemon's existing data/log directory convention and needs no root | unit + integration |
| REQ-8 | `trusty-search` wraps MCP handlers, embedder calls, and index queries in `latency` spans, enough to explain an 8-second query | integration (regression against the observed case) |
| REQ-9 | MCP schema change → rung 6 gate, with rung 5 failure-path coverage on the auto-off and sink-failure paths | process gate |

**Test-ladder rung.** This is an MCP schema change (three new tools) —
**rung 6** on `CLAUDE.md`'s Rust Test Ladder, at minimum rung 4 (cross-crate:
a shared library plus one daemon's public MCP surface). Given the new
process-lifecycle surface (a background sink, an auto-off timer), the
auto-off and sink-failure paths specifically get **rung 5's** failure-path
coverage. The implementing PR owes a changelog fragment for both
`trusty-telemetry` (new crate) and `trusty-search` (the pilot wiring), and
respects the 500-line production-file SLOC cap — split the sink, the MCP
handlers, and the category/atomics module into separate files from the
start rather than growing one file past the cap.

**Later phases** (filed when scheduled, per the epic's own convention):

| Phase | Content |
|---|---|
| 1.5 | `samply` trial (§8) — evaluation only, no crate code, blocks nothing |
| 2 | `memory` category from existing `sys_metrics`/`host_metrics`, plus low-frequency fd/index-size polls |
| 3 | Roll out to `trusty-memory`, `trusty-mpm`, `trusty-analyze` (cold-start spans for #8279), `trusty-embedderd` |
| 4 | `trusty-console` panel consuming `telemetry_summary`'s payload |
| 5 | `runtime` category, and the rebuild-only heavy tier (`console-subscriber`, `tokio-metrics` unstable fields, `dhat`, `pprof-rs`); a `telemetry_profile` control wrapping `samply`, contingent on the §8 trial passing |
| 6 | `tm` CLI verb, wrapping the same entry point §3's MCP tools call |

## 10. Open questions for an owner decision {#SPEC-TELEM-10~draft}

1. **Retention count for telemetry JSONL files.** §6's byte cap bounds one
   capture session; how many past sessions to keep before pruning is
   unresolved, pending whatever non-root rotation mechanism #8270 lands on.
2. **The samply-profile digest tool (§8).** No tool in this workspace today
   converts a Firefox Profiler JSON file into a bounded top-N hot-function
   digest an agent could read in place of the raw file. Whether this is a
   small new offline parser, or deferred indefinitely, is unsettled.

## Related

Research report:
[`docs/research/shared-telemetry-over-uds-2026-09-19.md`](../research/shared-telemetry-over-uds-2026-09-19.md).
Epic [#8295](https://github.com/bobmatnyc/trusty-tools/issues/8295), phase 1
[#8296](https://github.com/bobmatnyc/trusty-tools/issues/8296). Rotation
constraint: [#8270](https://github.com/bobmatnyc/trusty-tools/issues/8270).
Adjacent, independent work: [#6572](https://github.com/bobmatnyc/trusty-tools/issues/6572).
