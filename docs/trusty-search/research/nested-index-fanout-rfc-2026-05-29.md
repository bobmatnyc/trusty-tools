# RFC: Nested-Index Graph + Sub-Index Prioritization in Fan-Out Search

**Issue**: #404
**Date**: 2026-05-29
**Status**: DRAFT — awaiting maintainer sign-off on "Decisions Needed" section
**Dependencies**: #402 (relative chunk paths), #403 (.trusty-search/ co-located storage + filesystem discovery)
**Author**: Research analysis + RFC draft

---

## Summary

Today every index in trusty-search is a **flat peer** inside a `DashMap`. There
is no parent/child relationship, no dedup between overlapping indexes, and no
mechanism for the fan-out handler to know that "index B covers a subtree of index
A". This RFC proposes a **nested-index graph**: a directed acyclic hierarchy of
indexes where sub-indexes can be declared as children of a parent index, and the
fan-out search engine prioritises sub-index results for the covered subtrees
while using the parent only as a backstop. Flat peer indexes remain unchanged;
the feature is purely additive.

---

## Motivation

Three real operator scenarios drive this feature.

**Scenario 1 — Monorepo with a focused work subtree.**  
A large mono-repository is indexed as `project-root` (parent, 80 k chunks). The
developer's current work lives in `services/billing/`. They register `billing`
as a sub-index that deep-indexes only that subtree, with KG enabled, branch
boosting tuned, and embedding always warm. Fan-out queries should return
`billing` results first; the parent fills gaps for files outside the subtree.

**Scenario 2 — Company polyrepo rooted at a shared parent.**  
An aggregator index `all-repos` points at `/data/repos` and covers 12 sibling
repos. Each repo is also registered as its own named index. Today, a file in
`/data/repos/auth-service/src/middleware.rs` appears in both `all-repos` and
`auth-service`. Fan-out returns it twice with different chunk IDs. Users see
duplicates and duplicate context cost in LLM calls.

**Scenario 3 — Incremental specialisation without re-registration.**  
An operator wants to add KG expansion to a subset of an existing lexical-only
index without rebuilding the parent. They register the subtree as a child index
with full pipeline enabled. The parent stays lexical-only but the child provides
semantic + KG for that subtree.

---

## Current State

### Registry: flat `DashMap` of peers

`src/core/registry.rs`, `IndexRegistry` (line 623):

```rust
pub struct IndexRegistry {
    indexes: Arc<DashMap<IndexId, Arc<IndexHandle>>>,
}
```

Every registered index is a peer. `IndexHandle` carries `root_path` and
`include_paths` (absolute subtrees) but no `parent_id`, `children`, or
hierarchy metadata.

### Fan-out: `POST /search`

`src/service/server.rs`, `global_search_handler` (line 1872). Key steps:

1. **Lane construction** (lines 1878–1891): `registry.list()` returns all IDs
   as peers. The `indexes: [...]` restriction field filters to a caller-supplied
   subset.
2. **Routing** (lines 1906–1910): `RoutingMode::from_request` selects
   `All | TopN(n) | Threshold(f)` based on cosine-similarity of each index's
   `context_embedding` to the query vector.
3. **Concurrent per-index search** (lines 1956–1976): `join_all` across active
   IDs; errors are skipped and logged.
4. **RRF fusion** (lines 2006–2015): pairwise fold of all lanes using
   `rrf_fuse(&[], &lane, 1.0, 1.0, RRF_K, oversample)` — all lanes equally
   weighted in the `All` routing mode; score-weighted only by cosine similarity
   in `All` mode (line 1999).
5. **No dedup**: chunk IDs are namespaced as `{index_id}::{chunk.id}` (line
   1995), so a file present in two indexes produces two distinct entries in
   `chunk_lookup` and can appear twice in the final result list.

### `rrf_fuse` semantics

`src/core/search/rrf.rs` (line 28): rank-only fusion, ignores raw scores. The
accumulated RRF score for a document is `sum(weight * 1/(k + rank_i))` across
all lanes that contain it. A document appearing in two lanes accumulates score
from both. **Critical implication**: a chunk that appears identically in a
parent and a sub-index today would get double credit simply by being duplicated.
This is the primary dedup problem.

### Persistence

`src/service/persistence.rs`, `PersistedIndex` (line 31): flat TOML record with
`id`, `root_path`, `include_paths`, `exclude_globs`, and feature flags. No
parent/child field exists.

### `trusty-search.yaml` / repo config

`src/core/repo_config.rs`, `RepoConfig` (line 28): per-repo YAML that declares
named index slices (`IndexConfig`), each with `paths`, `exclude`, `languages`,
`domain_terms`, `skip_kg`. No hierarchy concept.

### `.trusty-search` marker / `.trusty-search.yaml`

`src/detect.rs` (line 56): the daemon detects a project root by walking up for
`.git` or a `.trusty-search` marker. `src/core/project_config.rs`: the
`.trusty-search.yaml` is a single-index shorthand used by the `index` CLI
command for simple projects. Neither file today supports declaring a parent
index.

---

## Open Questions and Recommended Answers

### Q1 — Nesting Detection

**Question**: how does the daemon determine that index B is nested under index A?

**Option A — Root-path prefix containment (implicit, automatic)**  
At fan-out time, sort all active index IDs by `root_path` length descending.
For any two indexes where `a.root_path` is a proper prefix of `b.root_path`,
treat B as a child of A. No schema changes. Pure runtime inference.

*Drawbacks*: fragile on symlinks. Cannot express "this index is a logical
subset of that one but not a filesystem subtree" (e.g., `include_paths` slice
of a non-contiguous set). Non-deterministic when two indexes have the same
`root_path`. Requires O(n²) path comparison at fan-out time.

**Option B — Explicit `parent_id` in `PersistedIndex` and `IndexHandle` (recommended)**  
Add an optional `parent_id: Option<IndexId>` to `IndexHandle` and to the
`PersistedIndex` TOML record. The child declares its own parent; the parent
does not need to enumerate its children. Children are discovered at startup by
iterating the registry. A child with a `parent_id` pointing to an unknown index
logs a warning and behaves as a peer (graceful degradation).

*Advantages*: precise, stable, survives symlinks, expressible in YAML and API.
Explicit operator intent. Zero ambiguity when two indexes share a `root_path`.

**Option C — `.trusty-search/` co-located config (depends on #403)**  
Once #403 lands, a `.trusty-search/index.toml` inside the child's root_path
could carry `parent = "<index-id>"`. This is discoverable without operator
re-registration: the filesystem scan at index time reads the file and
auto-wires the parent link.

*Drawbacks*: depends on #403 landing. Adds a new config file the operator
must commit.

**Recommendation**: Implement Option B (explicit `parent_id`) as the MVP.
Pursue Option C as a layer-on after #403. Option A is too fragile to ship.

---

### Q2 — Overlap / Dedup Semantics

**Question**: a file covered by both a parent index and a sub-index — return it
twice? which wins?

**Dedup key**: normalise the chunk's `file` field to an absolute canonical path
(resolving symlinks, stripping the index `root_path` prefix) and combine with
`(start_line, end_line)` to form a dedup key. Two chunks from different indexes
that resolve to the same `(canonical_absolute_path, start_line, end_line)` are
considered duplicates. Do **not** dedup by content hash — hash computation is
O(content length) and runs per result, which is too expensive during fan-out
fusion; and chunking parameters might differ between parent and child, producing
different chunk boundaries on the same file.

**Winner**: the sub-index always wins when there is a parent/child relationship
and the chunk falls inside the sub-index's covered paths. Rationale: the
sub-index was registered precisely because it offers better signal for that
subtree (fresher embed, KG enabled, finer chunking). The parent's copy is
dropped from the result set after fusion.

**Dedup timing**: post-RRF, before the final `truncate(top_k)`. Steps:

1. Run RRF fusion across all lanes (parent and sub-index lanes both participate
   to let RRF accumulate the full evidence set).
2. Walk fused results in score-descending order. For each result, compute its
   dedup key. If the key has already been emitted, skip this result; otherwise
   emit it and record the key.
3. The first occurrence (highest-score) wins. Because the sub-index is
   boosted (see Q4), its copy will ordinarily sort above the parent's copy,
   so "first wins" naturally picks the sub-index result without a separate
   identity check.

**Exception — disjoint files**: a file that exists in the parent but not in any
sub-index is not a duplicate and passes through unchanged. The parent index
serves as the "catch-all" for files not covered by any child.

This is the simplest correct approach and requires no cross-index path
resolution during indexing; it only runs during the fan-out result merge.

---

### Q3 — Cross-Index Score Normalisation

**Question**: how do scores combine across hierarchy levels given that parent
and sub-index are likely very different sizes?

**Current situation**: RRF is already rank-only (`rrf.rs` line 38–44); raw
scores from HNSW and BM25 are discarded before fusion. The `weight` factor
applied at fan-out (line 1999) is the cosine similarity of the index's
`context_embedding` to the query — not a chunk-level score. This means RRF
is already largely size-insensitive: a rank-3 result in a 100-chunk index and
a rank-3 result in a 100k-chunk index accumulate the same RRF contribution
`1/(60+3)` from that lane.

**What changes with nesting**: when a file exists in both parent (rank r_p) and
sub-index (rank r_c), both lanes contribute to the RRF accumulator for that
document's dedup key if we collapse them before RRF. But if we run separate
lanes (recommended), both chunks appear independently in the fused list and
are collapsed post-RRF by the dedup step.

**Proposed normalisation rule**: no special cross-level score arithmetic. The
size-insensitive property of RRF handles the hierarchy correctly as long as:

1. The sub-index lane is boosted by a flat multiplier `priority_boost` (see Q4),
   not by post-hoc score modification.
2. The `All` routing mode still applies cosine-similarity weighting to each
   lane independently; nesting does not change this.
3. The dedup step resolves ties after fusion, not before.

If a future measurement shows that very large parents consistently crowd out
sub-index results despite boosting (due to more unique chunks producing more
RRF accumulations), the mitigation is to increase `priority_boost` or to add
an optional `max_parent_fraction: f32` cap in the response (no more than X%
of the final top_k should come from a parent when a child covers the query
region). This is deferred to a follow-up ticket.

---

### Q4 — Priority Model

**Question**: what does "prioritize sub-indexes in fan-out" mean precisely?

**Proposed algorithm — sub-index lane boost**:

Each `IndexHandle` gains a new field `priority_boost: f32` (default `1.0`).
Sub-indexes declared with a parent may carry a `priority_boost > 1.0`
(configurable per-index; recommended default for sub-indexes: `1.5`, matching
the existing `branch_boost` default).

At fan-out lane construction (currently line 1993–2003 in `server.rs`), the
existing weight formula is:

```
weighted_score = chunk.score * cosine_weight
```

With this RFC the formula becomes:

```
effective_weight = cosine_weight * handle.priority_boost
weighted_score   = chunk.score  * effective_weight
```

In `All` mode this boosts sub-index chunks before they enter the RRF lane.
In `TopN` and `Threshold` modes the `cosine_weight` is 1.0 (selection has
already happened), so only `priority_boost` modulates the score.

**Boost values**:

| Index type | Default `priority_boost` |
|---|---|
| Root peer (no parent) | 1.0 |
| Sub-index (has parent) | 1.5 (operator-configurable) |

The operator can override `priority_boost` via `POST /indexes` and via the
`trusty-search.yaml` / `.trusty-search.yaml` `priority_boost` field.

**Alternative considered — sub-index-first then backfill**: collect top_k
results from sub-indexes first, then backfill from the parent for any
remaining slots. Rejected because it disables RRF fusion across levels: a
conceptual query about architecture might want results from both the sub-index
(specific implementation) and the parent (overview in a root-level README).
The boost approach keeps RRF's cross-lane aggregation intact.

---

### Q5 — Interaction with Existing Routing Modes

**`All` mode**: sub-index `priority_boost` multiplies the cosine-similarity
weight as described above. No other change.

**`TopN(n)` mode**: context-embedding cosine similarity is computed
independently for each index; sub-indexes typically have higher cosine
similarity to their specific domain queries, so they will naturally rank high.
The `TopN` filter runs before boost is applied (selection happens first, then
the selected lanes are boosted). Result: a highly-relevant sub-index is likely
to be selected even without boost; boost only matters after selection.
Recommended: keep this behaviour; do not modify `TopN` selection to account
for parent/child relationships.

**`Threshold` mode**: same reasoning as `TopN`. Sub-indexes with weak
`context_embedding` (e.g., they indexed a small config-only subtree) may fall
below the threshold even when they are the correct index for the query. To
mitigate: when a sub-index's cosine similarity is below threshold but its
parent's is above threshold, automatically include the sub-index as a bonus
lane with weight 1.0. This "child inclusion rule" ensures sub-indexes are never
silently excluded when the parent is in scope.

**`indexes: [...]` explicit restriction**: when the caller explicitly names
indexes, the nesting logic is bypassed entirely. The named set is treated as a
flat peer list. This preserves the existing precision-override semantics and
allows callers to opt out of hierarchy-aware fan-out completely.

---

### Q6 — API / Registry Changes

**Data model additions**:

1. `IndexHandle` (registry.rs line 169): add `parent_id: Option<IndexId>` and
   `priority_boost: f32`.
2. `PersistedIndex` (persistence.rs line 31): add `parent_id:
   Option<String>` with `#[serde(default, skip_serializing_if = "Option::is_none")]`
   for zero-churn backward compat.
3. `IndexConfig` (repo_config.rs line 36) and `.trusty-search.yaml`
   `ProjectConfig`: add `parent: Option<String>` and `priority_boost: Option<f32>`.
4. `POST /indexes` request body: add `parent_id: Option<String>` and
   `priority_boost: Option<f32>`.

**New registry helper**:

```rust
impl IndexRegistry {
    /// Return all children of `parent_id` in registration order.
    pub fn children_of(&self, parent_id: &IndexId) -> Vec<Arc<IndexHandle>> { ... }

    /// Return the parent handle of `id`, if one is registered.
    pub fn parent_of(&self, id: &IndexId) -> Option<Arc<IndexHandle>> { ... }
}
```

**`GET /indexes` response**: extend to include hierarchy metadata. New shape:

```json
{
  "indexes": [
    {
      "id": "project-root",
      "root_path": "/repos/project",
      "parent_id": null,
      "children": ["billing", "auth"],
      "priority_boost": 1.0
    },
    {
      "id": "billing",
      "root_path": "/repos/project/services/billing",
      "parent_id": "project-root",
      "children": [],
      "priority_boost": 1.5
    }
  ]
}
```

The existing `indexes: ["id1", "id2"]` flat format must remain available as a
backward-compatible alias via a `flat: true` query parameter.

**`GET /indexes/:id/status`**: add `parent_id`, `children`, and `priority_boost`
fields to the response.

**`POST /search` response**: add `hierarchy_dedup_count: usize` field indicating
how many chunks were dropped by the dedup step, for operator observability.

---

### Q7 — Migration / Backward Compat

**Zero breaking changes for existing deployments**:

- All new fields (`parent_id`, `priority_boost`) use `#[serde(default)]` and
  `skip_serializing_if`. An existing `indexes.toml` loads and behaves identically.
- An index registered via the old `POST /indexes` API (without `parent_id`) is
  a root peer with `priority_boost = 1.0` — identical to today's behaviour.
- `GET /indexes` currently returns `{ "indexes": ["id1", "id2"] }`. The new
  shape changes this to an array of objects. This IS a breaking change for
  existing callers. Mitigation: add a `version=2` query parameter, or rename
  the response key. Recommended: keep the existing `"indexes": [string]` shape
  under a `flat=true` query param (default `true` for one release, then flip).
  OR: bump the `GET /indexes` response to `version: 2` with the object array,
  and let the old flat format live at `GET /indexes?format=flat`. The decision
  is flagged in the "Decisions Needed" section.
- The MCP `list_indexes` tool similarly returns a flat list today. Add
  `list_indexes_v2` or extend with an `include_hierarchy: bool` parameter.
- The file-watcher registration path (`src/service/watcher.rs`) constructs
  `IndexHandle::bare(...)`. It must be updated to propagate `parent_id = None`
  and `priority_boost = 1.0`, which is already the `IndexHandle::bare` default —
  no action required.

---

### Q8 — Phasing

**Phase 1 — Data model + registry (prerequisite for everything)**

- Add `parent_id: Option<IndexId>` and `priority_boost: f32` to `IndexHandle`
  and `PersistedIndex`.
- Add `children_of` / `parent_of` helpers to `IndexRegistry`.
- Accept `parent_id` and `priority_boost` in `POST /indexes`.
- Persist and warm-boot both fields.
- No fan-out behaviour change yet. Tests: TOML round-trip, registry helper
  correctness.

**Phase 2 — Fan-out boost**

- Apply `priority_boost` to lane weights in `global_search_handler`.
- Apply "child inclusion rule" for `threshold` routing mode.
- Add `hierarchy_dedup_count` to the search response.
- Tests: fan-out with a parent+child pair, verify child results rank higher for
  files in the child's subtree.

**Phase 3 — Dedup**

- Implement post-RRF dedup by canonical `(absolute_path, start_line, end_line)`.
- Requires `root_path` to be available at dedup time (it is — `IndexHandle`
  carries it and chunks carry `file` as a relative path from `root_path`).
- Requires #402 (relative chunk paths) to be landed first so the canonical path
  resolution is reliable.
- Tests: same file in parent + child → appears once in output, tagged with the
  child's index_id.

**Phase 4 — `GET /indexes` hierarchy response + MCP update**

- Extend `GET /indexes` to include hierarchy metadata.
- Update MCP `list_indexes` tool with opt-in hierarchy field.
- Update admin Svelte UI to render the tree.

**Phase 5 — `.trusty-search/` auto-wiring (depends on #403)**

- Once #403 lands, the filesystem discovery scan reads `parent` and
  `priority_boost` from `.trusty-search/index.toml` and auto-registers the
  parent link on index creation.

**Minimum viable slice**: Phases 1 + 2 + 3 deliver the core value. Phase 4 is
observability. Phase 5 is ergonomics. Do not attempt to ship Phases 2–3
without Phase 1 as a committed, tested foundation.

---

## Proposed Data Model

### `IndexHandle` additions (registry.rs)

```rust
pub struct IndexHandle {
    // ... existing fields ...

    /// Parent index ID, if this index is a sub-index of another.
    ///
    /// Why: encodes the parent/child relationship for fan-out dedup and
    /// priority boosting without requiring filesystem path arithmetic at
    /// query time.
    /// What: `None` for root peers; `Some(id)` for sub-indexes. Set once at
    /// `POST /indexes` and persisted. The registry validates that the parent
    /// exists at registration time (warn-and-accept if not found, to tolerate
    /// registration ordering).
    /// Test: `registry_children_of_returns_registered_children` and
    /// `parent_of_returns_none_for_root`.
    pub parent_id: Option<IndexId>,

    /// Lane weight multiplier applied in fan-out search before RRF fusion.
    ///
    /// Why: sub-indexes need a configurable boost so their results rank above
    /// the parent's duplicate coverage without hard-coding 1.5 everywhere.
    /// What: `f32` in `[1.0, 4.0]`, clamped at registration. Defaults to
    /// `1.0` for root peers and to `1.5` for sub-indexes (when registered
    /// via `parent_id` without an explicit value). Persisted to `indexes.toml`.
    /// Test: `priority_boost_applied_in_fanout` integration test.
    pub priority_boost: f32,
}
```

### `PersistedIndex` additions (persistence.rs)

```rust
#[serde(default, skip_serializing_if = "Option::is_none")]
pub parent_id: Option<String>,

#[serde(default = "default_priority_boost", skip_serializing_if = "is_default_priority_boost")]
pub priority_boost: f32,

fn default_priority_boost() -> f32 { 1.0 }
fn is_default_priority_boost(v: &f32) -> bool { (*v - 1.0_f32).abs() < 1e-6 }
```

### `IndexConfig` additions (repo_config.rs / project_config.rs)

```rust
/// Parent index name. When set, this slice is registered as a sub-index.
#[serde(default, skip_serializing_if = "Option::is_none")]
pub parent: Option<String>,

/// Fan-out priority multiplier. Default 1.5 when `parent` is set, else 1.0.
#[serde(default, skip_serializing_if = "Option::is_none")]
pub priority_boost: Option<f32>,
```

### `POST /indexes` request body additions

```json
{
  "id": "billing",
  "root_path": "/repos/project/services/billing",
  "parent_id": "project-root",
  "priority_boost": 1.5
}
```

---

## Proposed Ranking Algorithm

### Fan-out lane construction (replaces server.rs lines 1992–2003)

```
for each active_index_id in active_ids:
    handle = registry.get(active_index_id)
    cosine_weight = weight_map.get(active_index_id).unwrap_or(1.0)
    effective_weight = cosine_weight * handle.priority_boost
    lane = per-index search results with each chunk.score * effective_weight
    lanes.push(lane)
```

### Threshold routing child-inclusion rule (new logic after RoutingMode::apply)

```
if routing_mode == Threshold:
    for each inactive_id (filtered out by threshold):
        handle = registry.get(inactive_id)
        if let Some(parent_id) = handle.parent_id:
            if weight_map.contains_key(parent_id):
                // parent is active → include this child at neutral weight
                add inactive_id to active_ids with weight 1.0
```

### Post-RRF dedup (new step between lines 2015 and 2017)

```
dedup_keys: HashSet<(PathBuf, usize, usize)> = {}
deduped_fused: Vec<(String, f32)> = []

for (namespaced_id, fused_score) in fused (score-descending order):
    chunk = chunk_lookup[namespaced_id]
    index_id = extract index_id from namespaced_id
    handle = registry.get(index_id)
    abs_path = handle.root_path.join(chunk.file).canonicalize() OR
               fallback to root_path.join(chunk.file) if canonicalize fails
    key = (abs_path, chunk.start_line, chunk.end_line)
    if key not in dedup_keys:
        dedup_keys.insert(key)
        deduped_fused.push((namespaced_id, fused_score))

hierarchy_dedup_count = fused.len() - deduped_fused.len()
fused = deduped_fused
```

`canonicalize()` failure (path does not exist on disk) is treated as a
non-fatal miss: use the un-resolved path as the key. This means a deleted-file
chunk that lives in two indexes will still be deduped by string match if the
paths are the same string, and will appear twice if they differ.

---

## Risks and Alternatives

### Risk 1 — `canonicalize()` latency in hot path

`std::fs::canonicalize` issues a stat syscall per path. For a top_k=20 result
set with a parent+child pair this is 40 stat calls in the search response path.
On a warm filesystem cache this is ~1 µs each, adding ~40 µs to the response.
Acceptable for now. Mitigation if it becomes a problem: cache `canonicalize`
results in a per-request `HashMap` keyed by the raw path string.

### Risk 2 — Cycle detection in the parent/child graph

An operator could register A as a parent of B and B as a parent of A. The
registry must detect and reject cycles at registration time. Use a DFS walk of
the `parent_id` chain: if registering C with `parent_id = A` would create a
cycle (A → B → C → A), return a 400 error.

### Risk 3 — RRF score inflation from double-lane participation

A chunk that appears in both parent and child lanes accumulates RRF score from
both, THEN is deduped post-fusion so only the higher-scored (child) copy
survives. This means the child copy's final score already includes the parent
lane's contribution, making it score higher than it would with only the child
lane. This is actually desirable — a chunk that appears in both is more
evidence of relevance — but operators should understand the mechanism.

### Risk 4 — Fragmentation of the `context_embedding` signal

Sub-indexes over small subtrees may have a weaker or noisier `context_embedding`
than the parent (fewer metadata files, less text for the fingerprint). This
could cause them to rank low in `TopN` and `Threshold` modes. The child
inclusion rule (Q5) partially mitigates this. A longer-term fix is to allow
sub-indexes to inherit or blend the parent's `context_embedding` — deferred.

### Alternative — Virtual "super-index"

Instead of a hierarchy stored in the registry, define a new `SuperIndex` type
that groups existing peer indexes and presents a merged view. The fan-out
handler checks if the search target is a `SuperIndex` and fans out over its
members with built-in dedup. Rejected: this would require a separate code path
for every operation (reindex, status, delete) and complicates the data model
more than `parent_id` + `priority_boost` does.

---

## Decisions Needed From Maintainer

These are the concrete choices that block implementation. Each needs a yes/no or
option selection before the first PR can be opened.

1. **Nesting detection mechanism (Q1)**: adopt explicit `parent_id` on
   `IndexHandle` + `PersistedIndex` (Option B), OR require waiting for #403 and
   using `.trusty-search/index.toml` (Option C), OR allow both in parallel?
   *Recommendation*: Option B for Phase 1, Option C as a later layer-on.

2. **Dedup key granularity (Q2)**: dedup by `(canonical_absolute_path,
   start_line, end_line)` as proposed, OR by chunk content hash (more
   expensive), OR by `{root_path-relative path}` string comparison only (cheaper
   but breaks cross-root dedup)?
   *Recommendation*: `(canonical_absolute_path, start_line, end_line)`.

3. **Default `priority_boost` for sub-indexes (Q4)**: use `1.5` (same as
   `branch_boost` default), or a different value? Should it be clamped, and at
   what max?
   *Recommendation*: `1.5` default, clamp to `[1.0, 4.0]`.

4. **`GET /indexes` response breaking change (Q6)**: add hierarchy metadata to
   the existing flat-string response (breaking) OR add a `?format=tree` query
   param that returns the object-array shape (non-breaking, additive)?
   *Recommendation*: `?format=tree` non-breaking additive variant.

5. **Child inclusion rule for `threshold` routing (Q5)**: when a sub-index's
   cosine similarity falls below the threshold but its parent is above, should
   the sub-index be automatically included? Yes/no?
   *Recommendation*: yes, include at weight=1.0 as a safety net.

6. **Cycle rejection policy (Risk 2)**: return HTTP 400 and reject cycles at
   `POST /indexes` registration time, OR warn and silently unlink the cycle-
   creating `parent_id`?
   *Recommendation*: 400 rejection — silent corruption is worse than a clear
   error.

7. **Dedup scope (Q2)**: dedup applies only when parent/child relationships
   exist, OR apply dedup to ALL fan-out results regardless of hierarchy (i.e.,
   dedup peer indexes that happen to share files too)?
   *Recommendation*: apply to all fan-out results unconditionally; it is
   strictly better behaviour even for flat peers.

8. **Phase ordering gate**: should Phase 3 (dedup) be gated behind #402
   (relative chunk paths) landing first, or can dedup ship using the current
   `chunk.file` field (which is already a root-relative path, but may vary by
   how the file was registered)?
   *Recommendation*: confirm that `chunk.file` is consistently root-relative
   before gating on #402. If it is, ship Phase 3 independently.
