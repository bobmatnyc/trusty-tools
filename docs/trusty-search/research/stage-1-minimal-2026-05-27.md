# Stage-1-minimal mode specification — 2026-05-27

**Status**: DRAFT — awaiting user review before implementation begins
**Tracking issue**: [#313](https://github.com/bobmatnyc/trusty-tools/issues/313)
**Target release**: trusty-search v0.17.0
**Spec author**: Rust engineer agent
**Reviewer status**: open — this document is the review gate

---

## TL;DR

A `lexical_only` reindex of the trusty-tools corpus (v0.14.0, cert #281) peaked
at **698 MB** and took **5,289 ms**. The symbol-graph (tree-sitter KG) rebuild
consumed 426 ms and produced 17,398 symbols + 229,814 edges. Because the KG is
built unconditionally even for `lexical_only: true` indexes, its construction
cost — and the petgraph heap it occupies — is paid whether or not graph search
is ever used.

This spec introduces a `skip_kg: true` additive flag on the persisted index
config that suppresses KG construction during a reindex. The expected outcome is
peak RSS < 200 MB and wall-clock < 3 s on the trusty-tools corpus. The flag
extends the existing `lexical_only` path; it is not a new index type.

---

## 1. Investigation summary

### 1.1 Where is the KG built in the current reindex pipeline?

The KG rebuild is a single unconditional call at the end of the batch loop in
`spawn_reindex_with_cleanup` (`crates/trusty-search/src/service/reindex.rs`,
line 1730):

```rust
// Phase 3: rebuild the symbol graph once for the whole reindex.
let kg = rebuild_symbol_graph_for_reindex(&handle).await;
mark_graph_ready(&handle).await;
```

`rebuild_symbol_graph_for_reindex` (lines 1038–1048) acquires the indexer's
READ lock, calls `indexer.rebuild_symbol_graph_now()`, then reads
`indexer.symbol_graph()` to extract node/edge counts. The underlying
`rebuild_symbol_graph_now` (in
`crates/trusty-search/src/core/indexer/ingest.rs`, line 392) delegates to
`rebuild_symbol_graph` (line 80), which:

1. Takes a read snapshot of every `RawChunk` in the in-memory corpus map,
   extracting `(id, file, function_name, calls, inherits_from, chunk_type)` into
   a `Vec<ChunkTuple>`. This snapshot is capped at `2 × kg_cap` entries to
   avoid O(corpus) allocations on huge indexes, but for a Medium-tier host
   (150,000-node cap) that can still be ~300,000 strings cloned in one shot.
2. Takes a read snapshot of the entity map (`entities`), similarly cloning all
   `RawEntity` records.
3. Calls `SymbolGraph::build_from_chunks_with_entities(&tuples, &entities)`,
   which builds a petgraph `DiGraph`. For 17,398 nodes and 229,814 edges the
   graph itself is roughly 50–100 MB of petgraph heap (node + edge indices are
   64-bit, each edge carries an `EdgeKind` enum and weight).
4. Persists the new graph to `CorpusStore` (redb write, ~10–20 ms).
5. Swaps the new `Arc<SymbolGraph>` into `symbol_graph: Arc<RwLock<Arc<...>>>`.

The two intermediate snapshots (`Vec<ChunkTuple>` and entity snapshot) are
allocated on the heap, used for graph construction, then dropped immediately
after `build_from_chunks_with_entities` returns. At peak, three structures are
simultaneously live: the old graph arc, the new graph being built, and the
snapshot vectors — which explains the memory spike at the tail of a
`lexical_only` reindex.

Importantly, **`function_name` and `calls` fields are always populated by the
tree-sitter chunker** (`crates/trusty-search/src/core/chunker/walk.rs`).
Neither the parse step nor `parse_files_only` (the `lexical_only` parse path in
`ingest.rs`, line 439) suppresses them. This means the chunk data needed for KG
construction is present regardless of `lexical_only`. Skipping the rebuild is
purely a runtime decision: "do not call `rebuild_symbol_graph_now`", not a
change to chunk contents.

### 1.2 Memory contribution breakdown

From cert #281 (v0.14.0 `lexical_only` reindex of trusty-tools):

| Phase | Time | Memory comment |
|---|---|---|
| Walk + hash filter | ~800 ms | Near-zero heap; paths only |
| Parse + BM25 batch loop | ~4,100 ms | Batched; peak per-batch, not cumulative |
| KG rebuild (tail) | **426 ms** | Two snapshots + petgraph all live together |
| Total | 5,289 ms | Peak RSS 698 MB |

The KG snapshot vectors for 17,398 symbols at ~100 bytes/tuple (id + file +
function_name + calls vec + chunk_type) = ~1.7 MB. The entity snapshot adds
another few MB. The petgraph `DiGraph` for 229,814 edges at ~48 bytes/edge
(source + target + weight struct) = ~11 MB. The residual RSS at 698 MB
(vs. expected ~80–120 MB for BM25 + redb alone) therefore traces to either the
node-cap snapshot allocation still in flight at the peak poll tick, or — more
likely — retained `RawChunk` strings from the prior batch loop that have not yet
been evicted from the idle-eviction cache. Regardless, removing the KG rebuild
entirely eliminates the 426 ms spike and the ~50–100 MB petgraph heap footprint,
and should drop peak RSS below 200 MB.

### 1.3 Persistence state of the KG for an existing `lexical_only` index

`rebuild_symbol_graph` saves the newly built graph to the `CorpusStore` redb
file via `graph_for_save.save_to_corpus(&corpus)` (ingest.rs, line 146). For an
existing `lexical_only` index that was previously built WITHOUT this flag, the
redb file already contains a persisted graph. Warm-boot
(`load_or_rebuild_symbol_graph` in `indexer/persist.rs`, line 220) loads that
persisted graph back into memory on daemon restart.

**This means**: a `lexical_only` index currently has a populated KG on disk
(from the unconditional Phase 3 rebuild), and warm-boot loads it. The search
handler gates `"kg"` out of `search_capabilities` because the `graph` stage
is `Skipped`, but the petgraph `DiGraph` object is alive in memory regardless —
it is just never queried via the search path.

---

## 2. API/flag shape decision

**Recommendation: Option (b) — additive `skip_kg: true` boolean on the
persisted config, with `--no-kg` as the CLI modifier.**

### Considered options

#### (a) New persisted index type: `"minimal"` or `"bm25_only"`

A third named type alongside `"full"` and `"lexical_only"` in `indexes.toml`.
This would require adding a new variant to `IndexMode` (or equivalent), new
serde paths, new CLI flags, and new staging logic across all touched structs.
The concept footprint is large for what is fundamentally a "skip one phase"
toggle.

**Rejected**: over-engineered for the use case. There is no functional
difference between a "minimal" index and a `lexical_only` index that also skips
the KG. We already have `lexical_only` as the stage-skip mechanism; we should
extend it rather than invent a parallel concept.

#### (b) Additive `skip_kg: true` + CLI `--no-kg` modifier (RECOMMENDED)

Add a single `skip_kg: bool` field to `PersistedIndex` and to `IndexHandle`.
When `true`, the reindex orchestrator wraps Phase 3 in an `if !handle.skip_kg`
guard. The `graph` stage is pre-marked `Skipped` at warm-boot when the field is
set. No index-type enum change needed.

`--no-kg` can be composed with `--lexical-only` or used independently:

```
trusty-search index ~/code/my-project --lexical-only --no-kg
trusty-search index ~/code/my-project --no-kg         # BM25 + KG suppressed; Stage 2 still runs (unusual)
trusty-search index ~/code/my-project --lexical-only  # existing behaviour unchanged
```

In practice, `--no-kg` is almost always paired with `--lexical-only` (that is
the Stage-1-minimal mode). It is exposed as an independent knob because there is
a theoretically valid use case for suppressing the KG on a full-pipeline index
in a memory-constrained environment.

**Accepted**: additive, backward-compatible, single field change.

#### (c) Make `lexical_only` imply `--no-kg`

Change `lexical_only` semantics so that KG is never built for those indexes.
This changes existing behavior for any operator who currently uses `--lexical-only`
and inspects `get_call_chain` results (rare, but possible). It also removes the
ability to re-enable KG on a `lexical_only` index without a schema change.

**Rejected**: semantic change to a stable flag. The correct approach is an
opt-in, not a silent behavior change.

#### (d) Hybrid: `--lexical-only` auto-enables `--no-kg`; `--with-kg` re-enables it

Invert the default for `lexical_only` indexes (no-KG by default), provide
`--with-kg` as the opt-in. This is the most aggressive memory-saving posture
but it is a breaking change for any `lexical_only` user who relies on the KG
being present (even if not queryable via search).

**Rejected**: breaking. Preference is explicit opt-in.

### Recommended CLI surface (option b)

```
# CLI flag on `trusty-search index` (and alias `reindex`)
--no-kg       # suppress KG construction during reindex; persisted to indexes.toml

# Typical Stage-1-minimal invocation:
trusty-search index ~/code/my-project --lexical-only --no-kg
```

**Default behavior**: unchanged. Neither existing `lexical_only` indexes nor
new full-pipeline indexes are affected unless `--no-kg` is explicitly set.

**Backward compatibility**: all existing `indexes.toml` files load correctly
because `skip_kg` will use `#[serde(default)]` and serialize only when `true`.

**Environment variable (none recommended)**: there is no compelling case for
`TRUSTY_SKIP_KG`. The flag is per-index, not machine-wide. Adding an env var
that silently suppresses KG for ALL indexes is a footgun.

---

## 3. Migration story for existing `lexical_only` indexes

Existing `lexical_only` indexes were built with KG on disk and (on warm daemons)
have the petgraph `DiGraph` loaded in memory. Three migration options:

**(A) Leave the KG bytes alone** — the on-disk `SymbolGraph` bytes and the
in-memory `Arc<SymbolGraph>` persist unchanged after upgrade. The graph stage
stays `Skipped` (already the case for `lexical_only`) so search never touches
it, but ~50–100 MB of petgraph heap survives until the next reindex or daemon
restart.

**(B) Delete the KG on first reindex with `skip_kg: true`** — when
`rebuild_symbol_graph` is skipped, also persist an empty graph to redb (or
delete the graph table entry) so the on-disk state matches the flag. Warm-boot
on the NEXT restart would find no graph and skip loading it, reclaiming the
warm-boot memory.

**(C) Require re-index to drop the KG** — no automatic cleanup; operators who
want the memory back simply run `trusty-search index --lexical-only --no-kg`
once. The persisted graph bytes in redb remain until that reindex overwrites them
(or the `skip_kg` path actively clears them during Phase 3).

**Recommendation: (C)** — require explicit re-index.

Rationale: Option A is the default (no new code, but wastes memory on warm
daemons that have already loaded the graph). Option B requires active cleanup
logic in the warm-boot path, adding complexity. Option C is the simplest and
most explicit: the operator invoking `--no-kg` is already declaring intent to
drop KG construction, so running that reindex once is the natural migration
step. The on-disk redb bytes for the graph are typically small (a few MB) and
the in-memory graph is evicted at the next daemon restart anyway.

For documentation clarity, the CLI help text for `--no-kg` should note: "If
this index previously had a KG on disk, re-running with this flag will remove
it from the corpus store."

---

## 4. Persisted config schema

### `PersistedIndex` struct diff

Location: `crates/trusty-search/src/service/persistence.rs`

```rust
// Before (last persisted field):
#[serde(default, skip_serializing_if = "std::ops::Not::not")]
pub lexical_only: bool,
```

```rust
// After: add below lexical_only
/// Stage-1-minimal mode (issue #313): when `true`, the KG rebuild
/// (Phase 3 of `spawn_reindex_with_cleanup`) is skipped entirely.
/// The graph stage is permanently `Skipped` at warm-boot.
///
/// Why: for pure BM25 / lexical deployments the petgraph DiGraph
/// can consume 50–100 MB of heap for a large corpus. Setting this
/// flag avoids building it at all, not just gating it at query time.
/// What: `#[serde(default)]` so older indexes.toml load as `false`;
/// only written to TOML when `true` to keep the file compact.
/// Test: `skip_kg_round_trips` in persistence tests.
#[serde(default, skip_serializing_if = "std::ops::Not::not")]
pub skip_kg: bool,
```

### `IndexHandle` struct diff

Location: `crates/trusty-search/src/core/registry.rs`

```rust
// Add after `pub lexical_only: bool`:

/// Stage-1-minimal mode (issue #313): when true, the KG rebuild is
/// skipped during reindex and the graph stage is permanently Skipped
/// at warm-boot. The petgraph DiGraph is never allocated.
pub skip_kg: bool,
```

### `indexes.toml` entry format

Before (current `lexical_only` index):
```toml
[[index]]
id = "trusty-tools"
root_path = "/Users/me/Projects/trusty-tools"
lexical_only = true
```

After (Stage-1-minimal):
```toml
[[index]]
id = "trusty-tools"
root_path = "/Users/me/Projects/trusty-tools"
lexical_only = true
skip_kg = true
```

Older daemon versions encountering `skip_kg = true` in their TOML will emit a
serde "unknown field" warning (toml crate default behavior) but will NOT fail to
load — the field is simply ignored and the index starts with KG enabled, which
is the safe fallback.

---

## 5. Search-time behavior

### Existing gating path (already handles `skip_kg`)

When `lexical_only: true`, `reset_stages_for_reindex`
(`service/reindex.rs`, line 484) pre-marks `semantic` and `graph` as
`StageStatus::Skipped`. `mark_graph_ready` (line 566) is a no-op when
`lexical_only`. The search handler reads `search_capabilities()` from
`IndexStages` — which only advertises `"kg"` when `graph.status.is_ready()` —
so no KG queries route to the symbol graph.

**For `skip_kg` alone (without `lexical_only`)**: `reset_stages_for_reindex`
must also pre-mark the `graph` stage as `Skipped` when `handle.skip_kg`. The
`mark_graph_ready` and `mark_semantic_ready_graph_in_progress` functions must
also become no-ops on `skip_kg` handles.

### Warm-boot: graph stage init

`build_stage_states_from_boot_inputs`
(`crates/trusty-search/src/commands/start.rs`, line 49) computes the initial
`StageState` from on-disk evidence. When `lexical_only: true`, it forces
semantic + graph to `Skipped`. The same branch must also fire when `skip_kg:
true` (for the graph stage specifically).

### In-memory symbol graph on `skip_kg` indexes

The `CodeIndexer` always initialises `symbol_graph` as an empty
`Arc<SymbolGraph>` (`SymbolGraph::new()`). When `skip_kg` is set, the graph is
never rebuilt, so it stays empty for the lifetime of the daemon. Because
`"kg"` is absent from `search_capabilities`, no search path will invoke
`snapshot_symbol_graph`. The `GET /indexes/:id/graph` and
`GET /indexes/:id/graph/health` endpoints already check
`search_capabilities` before dispatching — they should return a `503` or an
empty graph response. This is the same behavior as any index whose graph stage
is `Skipped`.

### Features silently disabled on `skip_kg` indexes

| Feature | Impact |
|---|---|
| `search_kg` MCP tool | Returns empty results (empty graph) |
| `GET /indexes/:id/graph` | Returns an empty node/edge list or 503 |
| `get_call_chain` | Returns empty chains |
| `search_capabilities` field | `"kg"` absent |
| KG-boosted RRF scoring | No KG expansion; pure BM25 + vector |

No new gating code is required for the search handler itself. The
`search_capabilities` contract already handles this: when `"kg"` is absent,
callers know not to issue graph queries.

---

## 6. Cert + acceptance

### Acceptance criteria

A passing cert must show:

- `peak_rss_mb` < 200 MB on a full `--force` reindex of the trusty-tools corpus
  with `--lexical-only --no-kg`.
- `kg_ms: 0`, `symbol_count: 0`, `edge_count: 0` in the SSE `complete` event.
- `vector_count: 0` (Stage 2 skipped, same as today with `--lexical-only`).
- `elapsed_ms` < 4,000 ms on the reference machine used for cert #281.
- All existing tests pass (`cargo test -p trusty-search`).
- `cargo clippy --workspace --all-targets -- -D warnings` clean.

### Cert invocation

```bash
# 1. Set an isolated data dir to avoid perturbing any live daemon
export TRUSTY_DATA_DIR=/tmp/ts-cert-313

# 2. Start a fresh daemon
trusty-search start

# 3. Register and reindex trusty-tools in Stage-1-minimal mode
trusty-search index /path/to/trusty-tools \
  --name trusty-tools \
  --force \
  --lexical-only \
  --no-kg

# 4. Inspect the SSE complete event for the metrics above
# (The CLI progress bar prints the complete event JSON on finish)

# 5. Stop the daemon
trusty-search stop
```

### Regression snapshot location

On passing, record a snapshot at:
```
docs/trusty-search/regression-testing/v0.17.0-{date}.md
```

The snapshot must carry the same columns as the v0.14.0 cert #281 entry:
`elapsed_ms`, `peak_rss_mb`, `kg_ms`, `symbol_count`, `edge_count`,
`vector_count`, `corpus`, `machine`.

---

## 7. Risks and open questions

### Q1 — Should `--no-kg` also suppress `function_name` / `calls` population in the chunker?

Currently the tree-sitter chunker always populates `function_name` and `calls`
on every `RawChunk`. These fields are used exclusively by the KG builder. If KG
is never going to be built, populating these fields wastes CPU during parse and
memory during the batch transit phase. Stripping them would reduce chunk size
by ~30–50 bytes per chunk (estimates vary by language and function name length),
or roughly 3–5 MB of transient heap for a 100,000-chunk corpus — not a
significant win.

**Decision needed**: is it worth the parse-path complexity to conditionally
suppress `function_name` / `calls` population? Recommendation is **no** for
v0.17.0 — the transient memory is negligible, and keeping the parse path simple
reduces risk. This can be a follow-up if profiling shows otherwise.

### Q2 — What happens to `get_call_chain` on a `skip_kg` index?

`get_call_chain` (`crates/trusty-search/src/service/call_chain.rs`) queries the
symbol graph. On a `skip_kg` index the graph is permanently empty, so all call
chains return zero hops. There is no dedicated error response for this case;
callers receive an empty result, which is technically correct but potentially
surprising.

**Decision needed**: should `get_call_chain` return a 503 / explicit
"KG unavailable" error on `skip_kg` indexes, or is an empty result acceptable?

### Q3 — Should `lexical_only: true` automatically imply `skip_kg: true` in a future version?

The original `lexical_only` documentation states the intent as "daemonized
ripgrep without embedder overhead." KG overhead was not on the radar when this
flag was introduced. Now that KG adds 426 ms and ~100 MB on a medium corpus, it
is inconsistent that `--lexical-only` does not also skip Stage 3.

**Open**: for v0.17.0 this is out of scope. If the opt-in `--no-kg` is widely
adopted, a future release could make `lexical_only: true → skip_kg: true`
implicit (with a deprecation cycle for the independent flag). User decision.

### Q4 — How does `--no-kg` interact with the `trusty-search.yaml` multi-index config?

The YAML-driven multi-index path (`handle_index` in
`crates/trusty-search/src/commands/index.rs`, line 248) hard-codes
`lexical_only: false` for YAML-driven registrations. The `--no-kg` CLI flag
similarly has no YAML equivalent today. For v0.17.0, `skip_kg` does not need
to be a YAML field; the CLI flag is sufficient. Future work: add `skip_kg:` to
the `trusty-search.yaml` schema if per-sub-project KG suppression is needed.

---

## 8. Implementation phases

All work targets a single PR, releasing as v0.17.0.

### Phase 1 — Config plumbing (S, ~1 hour)

- Add `skip_kg: bool` to `PersistedIndex` with `#[serde(default)]` /
  `skip_serializing_if`.
- Add `skip_kg: bool` to `IndexHandle` struct and `IndexHandle::bare()`.
- Update `POST /indexes` request deserialization in `server.rs` to thread
  `skip_kg` through to the handle.
- Update `spawn_reindex_with_cleanup` to read `handle.skip_kg` and produce a
  `KgRebuildOutcome { symbol_count: 0, edge_count: 0, kg_ms: 0, kg_skipped: true }`
  when set, bypassing `rebuild_symbol_graph_for_reindex`.
- Update `reset_stages_for_reindex` to pre-mark `graph` as `Skipped` when
  `handle.skip_kg`.
- Update `mark_graph_ready` to be a no-op on `skip_kg` handles.
- Update `build_stage_states_from_boot_inputs` to force `graph = Skipped` when
  `skip_kg`.
- Persist the flag correctly in `PersistedIndex` round-trip tests.

### Phase 2 — CLI wiring (S, ~30 min)

- Add `#[arg(long)] no_kg: bool` to the `Index` subcommand in `main.rs`.
- Thread `no_kg` through `handle_index` → `filters.skip_kg`.
- Update CLI help text.

### Phase 3 — Tests (S–M, ~1 hour)

- Unit test: `skip_kg_round_trips` in `persistence.rs` tests.
- Integration test: `no_kg_index_skips_phase3` in `service/reindex.rs` tests —
  mirrors `lexical_only_index_never_runs_stage_2`, verifies `kg_ms == 0`,
  `symbol_count == 0`, `kg_skipped == true` in the complete event, and
  `search_capabilities` excludes `"kg"`.
- Warm-boot test: `warm_boot_respects_skip_kg_flag` in
  `commands/start.rs` tests — mirrors `warm_boot_respects_lexical_only_flag`.

### Phase 4 — Cert run + regression snapshot (S, ~30 min)

- Run cert procedure from Section 6.
- Record `docs/trusty-search/regression-testing/v0.17.0-{date}.md`.
- Bump `crates/trusty-search/Cargo.toml` version to `0.17.0`.
- Update `CHANGELOG.md`.

**Total estimated effort**: M (3–3.5 hours of focused implementation). The
feature is a small config addition + one conditional guard in the reindex
orchestrator. Complexity comes entirely from ensuring tests and warm-boot paths
are consistent, not from the implementation itself.

---

## Decisions (locked 2026-05-27)

These answers were provided by the user after reviewing the open questions in
Section 7. They are binding for the v0.17.0 implementation.

### D1 — `lexical_only` / `skip_kg` coupling (Q3 in spec)

**Decision**: Keep the two flags independent in 0.17.0; revisit later.

`--lexical-only` does **not** imply `--no-kg`. The two flags are orthogonal
knobs: `--lexical-only` stops after Stage 1 (no embedding); `--no-kg`
suppresses the Phase 3 KG rebuild. Neither implies the other. This preserves
backward compatibility for any operator that uses `--lexical-only` and
inspects KG results (theoretically possible before 0.16.0 even though the
search capabilities gate hid them from search).

Future: once the flag combination `--lexical-only --no-kg` proves widely
adopted, a future release can consider making `lexical_only → skip_kg`
implicit, with a deprecation cycle.

### D2 — KG query behavior on `skip_kg` indexes (Q2 in spec)

**Decision**: Return a structured 503 / KG-unavailable error.

`get_call_chain` and other KG-dependent endpoints must return a **typed
error** indicating the KG was intentionally skipped, NOT a silent empty result.
Tooling and humans must be able to distinguish "no symbols found" from "KG
disabled."

Error contract (binding):
- **HTTP**: `503 Service Unavailable` with body
  `{"error": "kg_unavailable", "reason": "skipped_by_config", "index": "<id>"}`.
- **MCP / JSON-RPC**: error code `-32010` (reserved for KG-unavailable
  conditions in trusty-search), message `"KG unavailable: skipped_by_config"`.
- Affected endpoints: `GET /indexes/:id/call_chain` and `search_kg` MCP tool.
- The `search_kg` tool already uses the `STAGE_NOT_READY` error path when the
  `graph` stage is `Skipped`; the `skip_kg` path should produce the same
  `STAGE_NOT_READY` response (since `graph` is permanently `Skipped`).
  `get_call_chain` needs a new explicit 503 guard since it currently falls
  through to an empty graph result rather than an error.

### D3 — YAML config support (Q4 in spec)

**Decision**: Include `skip_kg` as a first-class field in `trusty-search.yaml`
in 0.17.0. CLI and YAML must stay in sync — both surfaces support `skip_kg`
from day one.

`IndexConfig` in `repo_config.rs` gains a `skip_kg: bool` field (default
`false`, `#[serde(default)]`). The multi-index YAML path in
`commands/index.rs` threads `filters.skip_kg` the same way it threads
`filters.lexical_only`.

### D4 — Environment variable (not in original Q, added for completeness)

**Decision**: Add `TRUSTY_NO_KG=1` env var as a machine-wide override (despite
the original spec recommendation against it). Rationale: the user specified
this in the implementation plan. `TRUSTY_NO_KG` sets `skip_kg: true` globally
when the env var is `"1"`, `"true"`, or `"yes"` (case-insensitive). It is
treated as a default that can be overridden per-index by the YAML field.

### Consequent spec updates

- **Section 2 (API/flag shape)**: the env-var `TRUSTY_NO_KG` is now supported
  as a machine-wide default (contrary to the earlier "none recommended"
  recommendation; user decision overrides).
- **Section 5 (Search-time behavior)**: `get_call_chain` returns 503
  `kg_unavailable` rather than silently returning an empty graph. The MCP
  `search_kg` tool already produces `STAGE_NOT_READY` when `graph = Skipped`;
  no additional change needed there since `skip_kg` pre-sets `graph = Skipped`.
- **Section 7, Q2**: resolved by D2 above.
- **Section 7, Q3**: resolved by D1 above.
- **Section 7, Q4**: resolved by D3 above.
