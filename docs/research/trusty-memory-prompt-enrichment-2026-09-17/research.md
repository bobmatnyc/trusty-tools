# Graph prompt enrichment: current source and experiment seams

Source-only review of `/Users/masa/trusty-search-experiment/worktree`, 2026-09-17. No live memory, build, Git mutation, or ticket operation. Applied documentation-style skill. `trusty-search search` reports `index 'worktree' not found on daemon`; direct source search is authoritative. File references below are relative to the worktree.

## Findings

**The implementation already contains quick resident graph lookup, but the prompt enrichment path does not use it.** Standing facts and query-dependent graph enrichment are separate paths.

| Path | Actual mechanism | Source |
|---|---|---|
| Standing facts | `get_prompt_context` clones resident `PromptFactsCache`; no query serves preformatted Markdown; query applies a case-insensitive whole-string substring test over subject/object | `crates/trusty-memory/src/tools/kg_ops.rs:409` |
| Cache rebuild | Gathers active rows across every registered palace, takes newest 1024 per palace before hot-predicate filtering, then formats and atomically replaces cache | `crates/trusty-memory/src/prompt_facts.rs:328`, `:593` |
| Prompt hook KG | Calls `memory.kg_all` with limit 200, then applies hot-predicate allowlist and lexical prompt overlap, takes first top-k | `crates/trusty-memory/src/commands/prompt_context/fetch.rs:36`, `:117`; `filter.rs:205`; `mod.rs:464`, `:494` |
| KG all | Full TRIPLES scan, discards history/closed rows, sorts valid_from descending, only then applies limit/offset | `crates/trusty-memory/src/service/core_kg.rs:203`; `crates/trusty-common/src/memory_core/store/kg_redb/read_ops.rs:146` |
| Exact subject | redb subject-prefix range, filters closed rows; no entire-table scan | `crates/trusty-common/src/memory_core/store/kg_redb/read_ops.rs:39` |
| Resident traversal | HashMap entity-to-node plus StableGraph adjacency, hydrated once on open and maintained by writes | `crates/trusty-common/src/memory_core/store/kg/adjacency.rs:30`, `:185` |

Thus a 200-row response limit does not bound storage work. Recent structural triples can crowd out old useful hot facts before filtering. The prompt hook's current graph lane is lexical filtering over a recently sorted graph page, not relationship traversal. This is a source-derived mechanism finding, not measured production latency.

## Reusable public APIs

Use `trusty_common::memory_core::store::kg::{KnowledgeGraph, Triple, ExpandDirection}` with an explicitly supplied temporary directory and synthetic data. Keep one opened graph resident throughout measured queries; separately record open/hydration and construction costs.

- `KnowledgeGraph::open` / `open_with_intent`: `kg/graph.rs:129`, `:145`. Path is transformed by `redb_path_for` at line 101; ensure directory cannot resolve to a live palace.
- `assert_sync`: `kg/ops.rs:464`; delegates to `KgWriter::assert_sync`, which writes the store directly (`kg_writer.rs:380`) without updating resident adjacency. After synchronous or bulk fixture population, drop and reopen the graph, then verify expected neighbors before timing. The async `assert` path performs adjacency synchronization; do not assume the synchronous path is equivalent.
- `query_active(subject)`: `kg/ops.rs:29`; exact prefix lookup of outgoing facts. Good resident-versus-storage comparator.
- `neighbors(entity)`: `kg/graph.rs:342`; incoming and outgoing adjacent edges without redb I/O. Returns `(other_entity, KgEdge)`, **loses direction** in its return shape. Do not format these as directed statements without recovering direction.
- `expand_neighbors(entity, direction, max_hops)`: `kg/explore.rs:173`; resident BFS and correctly directed full triples. Prefer this when preserving subject/object for prompt evidence.
- `shortest_path(from,to)`: `kg/graph.rs:385`; node-name path only, directed Dijkstra. It does not return provenance-bearing edges. Not needed for a first prompt test.
- `dump_all_triples`: `kg/ops.rs:499`; supports building an offline temporal fixture projection including history. Avoid using per query.
- `prompt_facts::build_prompt_context` / `hot_fact_bullet`: `prompt_facts.rs:237`, `:230`; pure standing-fact formatter. It intentionally drops subject for `is_fact` and `has_convention`; use another evidence-card formatter when source identity is required.

## Scope, time, provenance, and bounded work

1. Standing cache merges all palaces and retains only `(subject,predicate,object)`. Neither cache nor returned `TierSFact` carries palace/source identity (`prompt_facts.rs:49`, `:149`, `:328`). `handle_list_prompt_facts` ignores its `_args` (`tools/kg_ops.rs:183`). Do not infer palace filtering from the tool call's arguments. Scoped benchmark cache keys must include scope.
2. Active storage/adjacency means `valid_to == None`; `query_active` does not compare `valid_from` to a query clock (`kg_redb/read_ops.rs:39`). `expand_neighbors` has no as-of/TTL/cutoff parameter or per-edge time filter (`kg/explore.rs:173`). Future-effective assertions therefore need experiment-side filtering. Historical retrieval cannot rely on active-only adjacency.
3. `KgEdge` carries confidence, provenance, and valid interval (`kg/types.rs:39`), but no drawer revision, source digest, byte range, observed-at, or verified-at. `auto:remember` is generic extractor provenance (`kg_extract.rs:12`, `:79`); it does not prove a precise source span. Keep fixture evidence mapping external and mandatory.
4. `expand_neighbors` bounds hops, **not degree or examined edges**, and computes full graph degree for each reached node (`kg/explore.rs:173-269`). A one-hop hub can still return huge output. Calling it then truncating bounds payload but not work. Compare that existing API honestly; a new bounded resident projection needs explicit max seeds, scanned edges, emitted edges, depth, and tokens, with truncation flags.
5. Traversal output order follows graph insertion order. Sort stable identities and define deterministic tie breaks before bounded selection. Test insertion-order perturbation, cycles, duplicate paths, empty/unknown subjects, high-degree hubs, and ambiguous aliases.
6. Cache rebuilding uses active row `valid_from` as `affirmed_at` (`prompt_facts.rs:289-335`), including automatic promotions. An index dream cycle must not reassert unchanged source facts and accidentally refresh this timestamp. Derived cache generation time is separate from factual freshness.
7. Committed storage can diverge from adjacency on failed sync, explicitly represented by `AdjacencyDesync` (`kg/types.rs:49`). Test update/retract/reopen consistency. Do not benchmark stale adjacency as accepted behavior.

## ADR status versus current source

ADR-0028 prescribes separately budgeted standing/current/episodic content. Standing hot predicates remain exactly `is_alias_for`, `has_convention`, `is_fact`, `is_shorthand_for` (`prompt_facts.rs:63`). Cap is 20 facts and 80 Unicode scalar values per object (`:85`, `:99`); that is not a strict 1600-byte/token cap.

ADR-0038 keeps graph additive and calls for same-budget content comparison. Its historical blockers must not be repeated as current facts:

- `(subject,predicate,object)` key and parallel adjacency retention are implemented (`kg_store.rs:20`, `kg/adjacency.rs:94`).
- Subject enumeration and distinct graph-empty versus unknown-subject query responses exist (`tools/kg_ops.rs:257`, `:341`).
- Stopword normalization/filtering exists (`kg_extract.rs:343`, `:622`). This establishes a mechanism, not extraction precision on real memories.

The old ADR's corpus counts and vendor survey are dated observations, not current measurements. Its caution remains useful: curated oracle edges test potential; deterministically extracted edges test achievable quality. Report both distinctly.

## Recommended experiment additions

Parent owns final design. Test two questions independently: **does exact scoped resident graph lookup reduce enrichment work**, and **does relation-derived evidence improve a fixed-size prompt packet**? Reuse the actual resident graph API for the first. For the second compare baseline, graph, embeddings, combined with identical output budgets and explicit fact-level evidence labels. Include relationships whose target terms do not occur in the query, inverse direction, two-hop chains, alias ambiguity, false links, superseded edges, cross-scope duplicate names, negative controls, and hubs. Keep standing rules as a separately measured fixed packet rather than allowing BM25 relevance to decide their presence.

A bounded deterministic projection can be built at consolidation time from explicit source facts, keyed by scope/entity/predicate with source revision and validity retained. It should update incrementally and allow old stores to continue operating. This is a proposed experiment seam, not an implementation claim.

## Verification and handoff

Source inspection only; no runtime speed or quality claim. Parent continues experiment design and implementation. No repo edits. Research saved at `/tmp/graph-prompt-research.md`.
