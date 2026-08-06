# Cost/Value: Migrating `trusty-common::memory_core` Storage from redb to Lance

**Date**: 2026-08-06
**Status**: Research complete — recommendation: **DON'T MIGRATE**

## Prior work this document builds on

This is not the first pass at this question. Read these before re-deriving
anything below — the relevant conclusions are cited inline rather than
repeated:

- `docs/trusty-agents/research/tantivy-surrealdb-memory-evaluation.md` —
  2026-04-25 evaluation of Tantivy/SurrealDB against the same redb+usearch
  stack. Recommended phased, targeted additions over a wholesale replatform,
  and flagged embedded-database startup latency and binary-size cost as the
  primary risk of swapping the storage engine.
- `docs/adr/0018-loopback-only-doctrine.md` — every trusty-* daemon is
  loopback-only; only `trusty-console` is reachable off-machine. Object-storage
  backing is not a capability this architecture wants.
- `docs/adr/0027-rooms-are-real-wings-are-scopes-closets-are-an-index.md` and
  `docs/adr/0028-memory-recall-tiers-standing-current-episodic.md` — both
  measured against the live estate (94 palaces, 1.4 GB, 1,531+ drawers across
  cached palaces, largest palace 1,039 drawers / 9,362 triples / 1,005
  vectors) and both are **additive-only over existing redb tables** — the
  operative migration-safety doctrine this document inherits: no existing
  row is ever rewritten or deleted for a schema change.
- `docs/trusty-search/decisions/0001-bundled-embedder-sidecar.md` and
  `docs/trusty-search/research/bm25-memory-2026-05-28.md` — prior memory-
  footprint investigations. Neither identified redb's on-disk row format as
  the RSS driver; the causes found were an in-process BM25 inverted index,
  ONNX arena allocation, and eager warm-boot loading — none of which a
  storage-engine swap touches.
- **GitHub issue #4990** (`roadmap: adopt four design ideas from
  omnigraph.dev for memory-core KG and search fusion`) — the repo's own prior
  research pass on this exact comparison, opened and closed with a verdict of
  **WATCH**, not adopt: "its featured deployment is S3/Lance-backed, against
  the loopback-only, local-first doctrine (ADR-0018)." Its explicit line on
  Lance: *"Lance/columnar object-storage backing points at the same problem
  as the trusty-search heap work (eager warm-boot drove the 13-16 GB
  footprint). Noted, not a recommendation."* This document reaches the same
  conclusion with the full evidence trail #4990 didn't have room for.

## 1. What `memory_core` stores today, and where

Every palace is `<data_root>/<palace_id>/`, holding **four separate redb
database files**, each with its own exclusive flock (`crates/trusty-common/
src/memory_core/registry.rs:62-64`, `retrieval/handle.rs:38,308,312,387`):

| File | Tables (all defined in `store/kg_store.rs`) | Opened by |
|---|---|---|
| `kg.db` | `TRIPLES`, `TRIPLES_BY_OBJECT`, `TRIPLES_BY_PREDICATE`, `ACTIVE_SUBJECT_COUNTS`, `DRAWERS`, `DRAWERS_BY_FACT_KEY`, `ROOMS`, `ROOM_KEYS`, `WINGS`, `WING_KEYS`, `PAYLOADS` | `KgStoreRedb` (`store/kg_redb/store.rs`) |
| `index.usearch.redb` | `VECTORS`, `VECTOR_KEYS`, `DELETED_VECTORS` | `HnswStore` (`store/hnsw_store.rs`), wrapped by `UsearchStore` (`store/vector.rs`) |
| `recall.redb` | `RECALL_LOG` | `RecallLog` (`analytics.rs`) |
| `chat_sessions.redb` | `SESSIONS` | `ChatSessionStore` (`store/chat_sessions/store.rs`) |

Twelve logical tables across four files, all byte-slice-keyed redb
`TableDefinition`s with postcard-encoded values (`store/kg_store.rs:19-194`).
`DrawerRecord` (the "memory" unit — content, tags, importance, room, TTL,
fact-slot) is versioned through four on-disk shapes decoded via a fallback
chain (`store/kg_redb/types.rs:40-198`) — postcard is positional, so every
field added since #61/spec-001/#4884 required a new intermediate struct
rather than a schema migration.

## 2. Access patterns

### Reads

| Pattern | Example | Path |
|---|---|---|
| Point lookup by key | Drawer by UUID; room by canonical key; vector-id by drawer UUID | `DRAWERS.get(uuid)`, `ROOM_KEYS.get(key)` (`store/kg_store.rs:57,102`), `VECTOR_KEYS.get(uuid)` (`hnsw_store.rs:337`) |
| Prefix scan | All triples for a subject; all payloads in a segment; closed/superseded triple history | `subject_prefix()` / `segment_prefix()` (`store/kg_store.rs:331,358`); superseded triples are kept as closed-interval rows rather than deleted (`store/kg_redb/write_ops.rs:37-262`, cited directly in issue #4990's finding 2) |
| Full table scan | Every drawer at palace open (hydration); every `VECTOR_KEYS`/`VECTORS` row on HNSW rebuild | `retrieval/handle.rs` open path; `hnsw_store.rs:184-283` (`HnswStore::open_with_mode` replays the entire `VECTORS` table into a fresh in-memory graph on every open) |
| Vector similarity | Cosine ANN over the in-memory `hnsw_rs` graph | `hnsw_store.rs:370-419` |

Frequency, measured (ADR-0028 §C1): **8,063** prompt-context injections over
31 days for one project alone, **99.75%** of which include a ranked-drawer
(vector + KG) recall section. That is a read on every `UserPromptSubmit`
hook across every active session — the read path is the hot path, and it is
dominated by point lookups (drawer-by-id after an ANN hit) and small ranked
scans, never a multi-million-row analytical scan.

### Writes

A single `remember_with_options` call (`retrieval/handle.rs:535-725`) — the
entry point behind `memory_remember` / `memory_note` / `task_add` — touches,
in order, under one per-palace write mutex (`timeouts::lock_with_timeout`,
`handle.rs:570-575`):

1. Room resolution: a `ROOM_KEYS` read, and on a new room a write to
   `ROOMS` + `ROOM_KEYS` in `kg.db` (`handle.rs:611-612`).
2. Embedding (external ONNX call via the shared embedder — not storage).
3. Vector upsert: one write transaction touching `VECTORS`, `VECTOR_KEYS`,
   `DELETED_VECTORS` in the **separate** `index.usearch.redb`
   (`hnsw_store.rs:315-355`).
4. Drawer persistence: one write transaction touching `DRAWERS` and, when a
   `fact_key` slot is claimed, `DRAWERS_BY_FACT_KEY`, in `kg.db`
   (`handle.rs:694-696`).
5. In-memory push to the drawer table (`handle.rs:698-705`).
6. **Full L1-cache rewrite** — every write re-sorts the whole in-memory
   drawer set and rewrites a top-15 JSON snapshot to disk (`handle.rs:715-
   718`, `store/l1_cache.rs`).
7. **Full closet-index rebuild** — every write re-tokenizes every drawer's
   content into an in-memory keyword→drawer-ids map (`handle.rs:722`,
   `rebuild_closets`, `handle.rs:769-780`).

A drawer's content is prose-sized (the estate's median is well under the
`INJECTION_BYTE_CAP` of 4,096 bytes carrying a whole ranked block, per
ADR-0028 §C11 — individual drawers are smaller still); its vector is a fixed
384×f32 = 1,536 bytes. This is small, frequent, single-row writes — the
opposite shape from `trusty-search`'s BM25 reindex, which the bm25-memory
research document measured at 23,513 chunks committed in one bulk pass
(`docs/trusty-search/research/bm25-memory-2026-05-28.md`, Part A). The two
subsystems share a storage engine but not a write shape, and `memory_core`
is the one under evaluation here.

## 3. Concurrency

`KgStoreRedb::open_with_intent` takes an `OpenIntent`
(`store/concurrent_open.rs:48-55`):

- **`Writer`** (the HTTP daemon, the sole writer): on `DatabaseAlreadyOpen`,
  retries with exponential backoff over a bounded ~1.55 s window
  (`WRITER_RETRY_ATTEMPTS=5`, `WRITER_RETRY_SLEEP_MS`, `concurrent_open.rs:83-
  103`) to absorb a graceful daemon-restart handoff, then **fails loud** — it
  never silently degrades to a read-only copy (issue #1487).
- **`ReadOnlyClient`** (CLI, stdio MCP): on lock contention, falls back to a
  **process-local whole-file copy** (`std::fs::copy`, `open_read_only_snapshot`,
  `concurrent_open.rs:404-428`) opened as an independent redb database, with
  writes against it rejected via `READ_ONLY_ERROR_MSG` (issue #59).

Within one process, `PalaceHandle`'s write path is further serialized by a
per-palace `write_mutex` with a bounded timeout (issue #906), and
`PalaceRegistry` bounds concurrently-open palace handles to an LRU cache of
64 (`registry.rs:70`, ~3-4 redb fds each) plus a per-palace open-lock so two
racing cold-opens of the same evicted palace never double-open the files
(issue #3992).

This concurrency model has real, currently-open bugs — but they are bugs in
the wrapper code, not evidence against redb as an engine:

- **#4911** (open): `OpenIntent::ReadOnlyClient` only takes the snapshot path
  when the file is already locked; on an *unlocked* file it opens read-write
  identically to `Writer`, and on an incompatible-format file it can rename
  the live store aside and hand back an empty one regardless of intent.
- **#5005** (open, P1): `HnswStore`'s vector-id allocator is seeded from an
  in-process `max(VECTORS, VECTOR_KEYS)+1` at open time and never persisted;
  a second process re-issues ids already in use, and `upsert` performs no
  uniqueness check — measured live, one `vector_id` in the `trusty-tools`
  palace is shared by 5 UUIDs, 4 of which are silently unreachable.

## 4. Where vectors live today

**Already isolated.** `VECTORS` / `VECTOR_KEYS` / `DELETED_VECTORS` live in
`index.usearch.redb`, a file entirely separate from `kg.db`
(`store/vector.rs:210-214`, `redb_path_for`). Vector storage already went
through one backend migration in-place — from a C++-FFI `usearch` index to a
pure-Rust `hnsw_rs` graph persisted in redb (issue #50/#51) — without
touching the KG or drawer tables. This matters directly for the
subset-migration question in §11: the boundary a partial migration would
need already exists as a file boundary today, and moving it does not, by
itself, buy any new isolation.

## 5. Current pain — is redb actually hurting?

| Evidence | Scope | What it shows |
|---|---|---|
| #4990 (this repo's own prior pass) | memory_core / KG | Explicitly evaluated Lance via omnigraph and declined: conflicts with ADR-0018, "not a recommendation." |
| #4911 (open) | memory_core, `concurrent_open.rs` | `ReadOnlyClient` open-path bug — wrapper logic, not a redb limitation. |
| #5005 (open, P1) | memory_core, `hnsw_store.rs` | Vector-id allocator has no persisted reservation or uniqueness check — an application-level bug in the pure-Rust HNSW wrapper, not a redb table-format issue. |
| #4639 (closed) | memory_core, `chat_sessions.redb` | Unbounded fd leak — a missing eviction policy in `session_stores: DashMap`, not tracked by the existing palace LRU. Fixed without touching redb. |
| #3659, #1870, #1158, #4333 (trusty-search corpus, a different subsystem) | trusty-search's own redb usage | Warm-boot lock contention, `DatabaseAlreadyOpen` races, and a misleading "corrupted format" status that conflates timeout with genuine corruption. Same storage engine, different code, same operational lesson: redb's failure modes here are about concurrent-open handling and status reporting, not row-level scalability or write throughput. |
| User memory: "trusty-search restart regresses large indexes" | trusty-search | A restart cost a 200k-chunk index to a 30 s redb open timeout under warm-boot contention — again an open/lock-handling issue, not a capacity ceiling. |

None of this evidence says redb cannot hold the data or serve the read/write
shape memory_core has. Every finding is about the *open/lock/allocator* code
built on top of redb. A storage-engine swap does not fix a single one of
them — Lance's own multi-process story (§7) is considerably less mature than
what these bugs are asking `concurrent_open.rs` to get right.

## 6. Lance on the merits

Verified against crates.io (2026-08-06, `User-Agent: trusty-tools-research`):

| Crate | Version | License | `rust_version` | Downloads (total / recent) | Last publish |
|---|---|---|---|---|---|
| `lance` | 9.0.1 | Apache-2.0 | 1.91.0 | 2,199,020 / 940,651 | 2026-08-06 (today) |
| `lancedb` | 0.33.0 | (inherits `lance`'s license family) | — (not independently checked) | 834,571 / 483,391 | 2026-07-28 |

`rust_version 1.91.0` is below this workspace's MSRV floor of 1.94, so no
compatibility blocker there. The version number itself is a signal worth
weighing: `lance` is at major version **9** after roughly four years on
crates.io (created 2022-07-28) — a materially faster breaking-change cadence
than redb's, which reached its current stable 2.x/4.x API without a
comparable major-version climb over the same period. This document did not
pull the full version-history changelog to quantify how many of those major
bumps were breaking for a typical caller; **that is an unverified unknown**,
flagged rather than assumed.

**What Lance genuinely gives that redb cannot:** columnar scans over large
datasets, a native vector index (IVF-PQ / HNSW) built into the storage
format rather than layered on top, a version/manifest chain that gives time
travel for free, zero-copy fragment sharing between readers, object-storage
(S3-compatible) backing, and lazy/on-demand fragment loading instead of
redb's whole-table hydration on open.

**What Lance is bad at for this workload, verified rather than assumed:**

- **Point lookups by key.** Lance's storage unit is a columnar fragment
  file; there is no B-tree-style single-key index analogous to a redb table
  row lookup. A point read resolves through the manifest to the owning
  fragment and then scans/decodes that fragment's relevant rows — a
  fundamentally heavier operation than redb's O(log n) in-page lookup for
  the drawer-by-uuid and room-by-key reads that dominate memory_core's read
  path (§2).
- **Small, frequent writes.** Every Lance write is a new fragment plus a new
  manifest version; there is no in-place row update. A `remember_with_options`
  call happening on the order of one drawer at a time (§2) would mint one
  fragment and one manifest version per call, requiring periodic `optimize`
  (fragment compaction) and `cleanup` (version-chain GC) as *mandatory*
  ongoing maintenance rather than the today's-nonexistent-because-
  unnecessary state of redb, whose single-file B-tree absorbs small updates
  in place.
- **Cross-table atomicity.** memory_core's 12 logical tables (§1) are
  mutated together inside a single redb write transaction for free — the
  `DRAWERS` + `DRAWERS_BY_FACT_KEY` pair in step 4 of §2 is one example.
  Lance's atomic unit is a single table's manifest CAS. Coordinating an
  atomic multi-table commit is exactly the problem omnigraph built an entire
  `__manifest` cross-table CAS layer to solve (§7) — it is not something
  Lance provides natively.
- **Single-file operational simplicity.** A redb palace backs up as "copy
  four files." Lance is a directory tree per table (data fragments +
  manifest + version files) needing explicit maintenance. Its local-disk
  mode is also **not durability-equivalent to its S3 mode**: upstream
  `object_store` leaves `PutMode::Update` unimplemented for
  `LocalFileSystem`, so a local Lance deployment emulates compare-and-swap
  with a content-token check-then-act rather than a true conditional put —
  documented directly by the omnigraph project itself
  (`docs/dev/invariants.md:685-693` in the cloned omnigraph repo, §7). A
  locked-loopback, local-first deployment (ADR-0018) would run Lance
  permanently in its structurally weaker mode.

## 7. Omnigraph as a worked example of the real cost

Omnigraph (MIT, Rust, github.com/ModernRelay/omnigraph — evaluated in #4990)
is a purpose-built multi-agent graph engine on top of Lance, and its own
`docs/dev/invariants.md` is the most concrete evidence available of what
"build a concurrent multi-table store on Lance" actually obliges a team to
build:

- A **`__manifest` cross-table CAS layer** — because Lance's own atomicity
  is per-table, omnigraph had to invent a second, higher-level compare-and-
  swap protocol to coordinate graph lineage (`graph_commit` / `graph_head`
  rows) against per-table version rows so a crash between the two can never
  be observed (invariants.md, "Manifest→commit-graph publish atomicity").
- **Numbered recovery-envelope generations** (v9 through v18 cited in the
  excerpts pulled for this document) — each a distinct crash-recovery
  protocol for a different multi-step operation (branch create/delete,
  optimize, streaming ingest), because a partially-applied Lance operation
  is not self-healing the way a redb transaction (all-or-nothing, no partial
  state ever visible) is.
- **`reclaim_orphaned_fork_and_refork` / `reconcile_orphaned_branches`** —
  dedicated reconciliation passes for branches left in an indeterminate
  state by a crashed or racing writer.
- **Explicit, non-automatic maintenance:** `cleanup` (version-chain GC,
  capped by the oldest version any live branch still depends on) and
  `optimize` (fragment compaction, index rebuild) are both operator-invoked
  commands, not background processes — the version chain and fragment count
  grow unbounded without them.
- **Copy-on-write branch semantics** riding directly on Lance's immutable
  fragments — the one piece of omnigraph's design that *does* port cleanly
  from Lance's properties, and the one capability (branch/fork a knowledge
  graph) memory_core has no present need for.
- A **documented single-writer-process support boundary that still holds**
  after all of the above: *"Lance exposes neither conditional native
  graph-branch create/delete nor compare-and-delete by `BranchIdentifier`
  for per-table refs, so a foreign process can mutate a ref between the
  final list/check and the operation. The documented single-writer-process
  support boundary remains until Lance provides a conditional ref primitive
  (or OmniGraph adds a distributed fence)."*

memory_core's concurrency model is daemon-as-sole-writer plus N read-only
snapshot clients (§3) — closer to omnigraph's supported single-writer-process
boundary than to Lance's raw multi-writer story, so it would not need
everything above. But it would need the cross-table CAS layer (§6's third
bullet — memory_core's tables are mutated together far more routinely than
omnigraph's branch model), some equivalent of the recovery-envelope
discipline (a crash mid multi-table Lance write is not automatically clean
the way a redb transaction abort is), and the mandatory `optimize`/`cleanup`
maintenance loop. That is not a mid-size migration PR — it is building a
smaller version of the exact infrastructure an independent, Lance-focused
project treats as its primary engineering investment, to serve a workload
(§2) that is a poor match for Lance's own strengths.

## 8. Cheaper alternatives that reach most of the value inside redb

| Lance benefit | redb-native path | Status |
|---|---|---|
| Time travel / version history | Triples already keep closed-interval history via non-destructive supersession (`write_ops.rs:37-262`). Drawers do **not** — `PalaceHandle::forget` hard-deletes with no tombstone (`handle.rs:790-850`, per #4990 finding 2) | Real gap, but it is a redb-native fix (add a tombstone/history table), already scoped as #4990 item 2 / issue #2869 — not a storage-engine question. |
| Native vector index | Already have it: pure-Rust `hnsw_rs` HNSW persisted in redb (`store/hnsw_store.rs`, issue #50/#51), already isolated in its own file (§4) | Solved. Open bug is the allocator (#5005), not the index. |
| Columnar scans / large-scale analytics | Not a workload memory_core has (§2) — the closest analogue, `trusty-search`'s BM25 corpus, is a separate crate whose own memory investigation (bm25-memory-2026-05-28.md) never identified redb's row format as the bottleneck | N/A — no gap to close. |
| Object-storage backing | Directly contrary to ADR-0018 (loopback-only, local-first) | Not wanted. |
| Schema enforcement at write time | Genuinely absent today (`kg_assert` takes three arbitrary strings, `crates/trusty-memory/src/tools/kg_ops.rs:22-81`) — but this is a validation-layer feature, orthogonal to the storage engine underneath it (#4990 item 3, the strongest of the four omnigraph ideas that DOES port) | Gap exists; the fix is a schema-validation layer in front of the existing write path, not a new database. |

Every Lance-specific benefit that memory_core would plausibly want either
already exists on redb, is already scoped as a redb-native fix, or is
actively excluded by standing architecture doctrine.

## 9. What we would lose

- **Single-file-per-database operational simplicity.** Backup, inspect, and
  reason about one file at a time; becomes a fragment+manifest directory
  tree per table requiring dedicated tooling to inspect.
- **Free cross-table atomicity.** Twelve tables mutated together inside one
  redb write transaction today become a hand-built cross-table CAS layer —
  the single most expensive thing omnigraph built (§7).
- **Cheap read-only snapshot fallback.** Today's `ReadOnlyClient` fallback on
  lock contention is a `std::fs::copy` of a sub-tens-of-MB file (§3) — fast
  at the estate's current scale (94 palaces, 1.4 GB). Lance's equivalent
  read path is a manifest-chain resolution against a store not optimized for
  single-row reads (§6).
- **A settled, structured error taxonomy.** `HnswStoreError` today wraps five
  distinct redb error types behind `thiserror` (`hnsw_store.rs:67-98`); an
  equivalent clean mapping for Lance's error surface was not verified here.
- **Fit with standing architecture doctrine.** ADR-0018 commits this
  workspace to loopback-only, local-first daemons. Lance's primary value
  proposition — object-storage backing — is exactly the capability that
  doctrine excludes.

## 10. Migration cost if attempted anyway

- **Data migration**: cheap in volume (94 palaces, 1.4 GB) but not in call
  sites — every consumer of `KgStoreRedb`, `HnswStore`/`UsearchStore`,
  `PayloadStore`, and `ChatSessionStore` (all of `trusty-memory`'s MCP/HTTP
  surface, `trusty-agents`' `TrustyBackedMemoryStore` adapter, `trusty-mpm`'s
  TUI health panel reading `PalaceInfo`) goes through `PalaceHandle` and
  would need its storage assumptions re-verified, not just recompiled.
- **API surface changes**: every read/write call site in §2 and §3 changes
  shape — point lookups become fragment-resolving reads, transactional
  multi-table writes become CAS-layer writes.
- **Test rewrite**: the existing `kg_redb`, `room_backfill`, `wings`, and
  `hnsw_store` test suites assume redb's transactional semantics
  end-to-end; none of that infrastructure carries over.
- **New operational burden**: mandatory `optimize` (compaction) and
  `cleanup` (version GC) loops that do not exist today, fragment-count
  monitoring, and object-store credential/config management even in local
  mode.
- **New failure modes**: CAS conflicts on the hand-built cross-table layer,
  the local `PutMode::Update` gap (§6), orphaned-fragment reclaim — an
  entirely new failure taxonomy layered on top of redb's already-open
  concurrency bugs (§5), not a replacement for them.

## 11. Verdict: DON'T MIGRATE

Every problem memory_core actually has today (§5) is a bug in the current
open/lock/allocator code sitting on top of redb, not evidence that redb
itself is inadequate for a small-scale (1.4 GB, low-thousands of drawers),
point-lookup-dominated, small-and-frequent-write workload. Lance's core
value proposition — columnar scans, object-storage backing, native ANN over
large fragment sets — is a mismatch for that shape on every axis checked in
§6, and every genuine Lance benefit is either already available on redb
(§8) or excluded by ADR-0018. The one thing Lance would unambiguously cost
is free cross-table transactional atomicity across memory_core's twelve
tables, replaced by a hand-built CAS/recovery layer — which is documented,
in detail, as the primary engineering investment of an entire independent
project (§7), not a side effect of a storage-engine swap.

**Single strongest reason:** the migration trades a solved problem (redb
already gives free cross-table atomicity and cheap point lookups for this
workload) for an unsolved one (building the equivalent of omnigraph's
`__manifest` CAS layer from scratch), to acquire capabilities (columnar
scans, object storage) the workload does not use and the architecture
doctrine (ADR-0018) excludes. Issue #4990, this repo's own prior pass on
the same question, reached the same conclusion with less detail: *"Noted,
not a recommendation."*

**No MIGRATE-A-SUBSET carve-out.** The most plausible subset — vectors only
— is already isolated in its own redb file today (§4) and already has a
working pure-Rust HNSW index (issue #50/#51); the estate's own open bug
against it (#5005) is an allocator defect, not a capacity or format
limitation. Moving `VECTORS`/`VECTOR_KEYS`/`DELETED_VECTORS` to Lance would
pay the same cross-process-CAS cost documented in §7 for a table set that
is already correctly scoped, at a per-palace vector count (low thousands)
far below the scale where a columnar ANN format earns back that cost.

## Unknowns

- Lance's exact breaking-change rate per major version was not quantified
  beyond the current version number (9.0.1) and the crate's four-year age;
  this document did not pull the full release history.
- Lance's error-handling ergonomics relative to today's `thiserror`-wrapped
  `HnswStoreError` (§9) were not independently verified.
- No load test was performed against either engine for this document;
  conclusions are architectural, drawing on omnigraph's documented
  production experience (§7) and the repo's own measured current scale.

## Sources

- `crates/trusty-common/src/memory_core/store/kg_store.rs`
- `crates/trusty-common/src/memory_core/store/kg_redb/{types,store}.rs`
- `crates/trusty-common/src/memory_core/store/concurrent_open.rs`
- `crates/trusty-common/src/memory_core/store/{hnsw_store,vector}.rs`
- `crates/trusty-common/src/memory_core/registry.rs`
- `crates/trusty-common/src/memory_core/retrieval/handle.rs`
- `docs/adr/0018-loopback-only-doctrine.md`
- `docs/adr/0027-rooms-are-real-wings-are-scopes-closets-are-an-index.md`
- `docs/adr/0028-memory-recall-tiers-standing-current-episodic.md`
- `docs/trusty-search/decisions/0001-bundled-embedder-sidecar.md`
- `docs/trusty-search/research/bm25-memory-2026-05-28.md`
- `docs/trusty-agents/research/tantivy-surrealdb-memory-evaluation.md`
- GitHub issues: bobmatnyc/trusty-tools #4990, #4911, #5005, #4639, #3659,
  #1870, #1158, #4333
- crates.io API (`https://crates.io/api/v1/crates/lance`,
  `https://crates.io/api/v1/crates/lance/9.0.1`,
  `https://crates.io/api/v1/crates/lancedb`), fetched 2026-08-06
- Omnigraph clone, `docs/dev/invariants.md` (lines ~58-920, cited excerpts
  ~660-720 and ~685-693 for the manifest-CAS and local `PutMode::Update`
  findings), MIT license, github.com/ModernRelay/omnigraph
