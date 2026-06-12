# 0009. External-Extractor KG Ingest Contract — Durable Contributed Overlay in trusty-search

- **Status:** Accepted (2026-06-11 — maintainer acceptance in [#819](https://github.com/bobmatnyc/trusty-tools/issues/819); doc applied to main as `583d389`)
- **Date:** 2026-06-10
- **Scope:** Workspace-wide (`trusty-search` storage + query surface, `trusty-analyze` spillover + enrichment bridge, external extractors)
- **Supersedes / Superseded by:** —
- **Decided in:** issue [#819](https://github.com/bobmatnyc/trusty-tools/issues/819) (parent epic [#814](https://github.com/bobmatnyc/trusty-tools/issues/814), source Discussion #580)

> Location note: #819 originally prescribed `docs/trusty-analyze/research/`. This
> decision binds multiple crates plus external producers, so per the hybrid scope
> rule in `docs/adr/README.md` it is filed as a workspace ADR instead.

## Context

External extractors (the motivating producer is a T-SQL/C# extractor built on
Microsoft ScriptDom + Roslyn; future producers include endpoint/queue/config
scanners) emit **cross-tier relationship graphs** that the tree-sitter pipeline
cannot see: `proc/function/view/method → reads/writes → table`, `method →
calls → proc`, `table → references → table` (FKs). #819 asks where this data
ingests, in which of three shapes: (A) batch ingest into a core store, (B) a
federated MCP server per extractor, (C) a hybrid.

Ground-truthed constraints (all verified from source at `c74806d`):

1. **trusty-analyze has no durable graph.** Its only persistent store is the
   FactStore (one redb table of flat `(subject, predicate, object)` triples,
   linear-scan query, no traversal). The KG is recomputed from trusty-search
   chunks on every request; the SCIP overlay (`POST /indexes/:id/scip`) is
   in-memory only and lost on restart. The spec states "No graph persistence —
   graphs are recomputed per request" as a design position, not a gap.
2. **trusty-search has the only durable, traversable KG in the system**: redb
   `kg_nodes`/`kg_edges`/`kg_edges_rev` rehydrated into an in-RAM
   `petgraph::DiGraph` at warm boot. However, its **derived** KG is rebuilt
   from the chunk corpus on reindex, and its symbol identity is deliberately
   bare-name (#114) — one node per distinct symbol name.
3. **The #692 backing-store evaluation** (graph-store tier, 2026-06-10
   comment) concluded: keep redb-persisted + in-RAM petgraph behind the
   `KvStore` adapter; no dedicated graph engine is warranted (1-hop default,
   100k-node cap, traversal never touches redb at query time). Edge
   properties are carried by widening the petgraph edge weight type.
   Cross-tier queries measured in pilots are 2–3 hops — under the eval's
   4–5-hop "revisit" trigger.
4. **Neither daemon's C# call graph can anchor contributed identity.**
   trusty-search collapses methods to bare names by design (#114);
   trusty-analyze's call-edge targets are unresolvable against its own
   class-qualified node ids (~95% dangling, #913). Empirically, the
   extractor's `(file, Class.Method)` pairs join trusty-analyze's
   class-qualified method nodes at **87% (class-sound)** but join
   trusty-search's bare-name symbols only ambiguously. Conclusion: the
   contributed graph must be **identity-self-contained**; any host join is
   query-time enrichment, never a storage relationship.
5. **Pilot validation across both T-SQL archetypes** (same model, no
   special-casing): a proc-centric legacy codebase produced ~34.5k edges
   (86% data-flow — reads/writes/FK — requiring vocabulary that does not
   exist today; 13% call-graph expressible with existing edge kinds, incl. a
   ~1.1k-edge C#→proc bridge); an EF Core + SSDT codebase produced ~20.8k
   edges (95% data-flow; ~7.4k direct method→table from EF/embedded SQL,
   ~2.9k FK references, near-zero proc layer). The high-value queries ("what
   writes table X", "what does this method transitively touch", "callers of
   a deprecated proc") all resolve inside the contributed graph alone.
6. The epic's in-flight children already point at trusty-search:
   #817 adds `Reads`/`Writes`/`AccessesResource` as first-class
   `contracts::EdgeKind` variants with persistence round-trip; #818 adds a
   `Custom(String)` escape hatch; #816 hardens warm-boot handling of
   unknown edge-kind tags (version skew); #815 converges the diverged
   EdgeKind enums first.

## Decision

We will adopt **Option A — batch ingest — targeting trusty-search's persisted
KG as a durable contributed overlay**, with the following contract:

1. **Storage: contributed overlay tables, separate from derived tables.** New
   per-index redb tables (`kg_contrib_nodes`, `kg_contrib_edges`, plus the
   reverse-edge table) that the chunk-derived rebuild
   (`rebuild_symbol_graph`) never touches. Both table families merge into the
   single in-RAM petgraph at load/warm-boot. Reindex regenerates derived
   tables only; contributed data survives restart and reindex by
   construction.
2. **Vocabulary: ride the epic.** Data-flow edges use the #817 first-class
   kinds (`Reads`, `Writes`; FK = existing `References`; proc/UDF calls =
   `CallsFunction`); anything else uses `Custom(String)` (#818). New
   resource node kinds (`Table`, `View`, `StoredProcedure`, `Function`) are
   added once — they are new node types with zero collision against code
   symbols. #816's unknown-tag counter is the skew guard.
3. **Identity: self-contained, extractor-minted, never merged with host
   symbols.** Canonical ids: `database.schema.table` (lowercased,
   linked-server stripped to metadata), schema-qualified routine names,
   `Class.Method` + source file for host-language members. Idempotency: a
   node is its id; an edge is `(from, to, kind)`; re-ingest merges rather
   than duplicates.
4. **Ingest API: `POST /indexes/{id}/graph`** on trusty-search (JSON node/edge
   list with kind tags and metadata) plus an MCP tool equivalent. Batch
   semantics, invoked on demand or from CI. No trigger hook in core; the
   "when" belongs to the producer.
5. **Edge metadata** (provenance file, linked-server, confidence) is carried
   on the widened petgraph edge weight and persisted with the edge row, per
   the #692 evaluation's guidance.
6. **Non-graph residue spills to the trusty-analyze FactStore** (dynamic-SQL
   flags, confidence notes, free-form findings) — its natural shape; the KG
   carries only node→node relations.
7. **Query: one server-side traversal primitive** (`graph_neighbors`-style
   BFS endpoint + MCP tool: node, direction, edge-kind filter, max hops)
   over the merged graph. Optional **query-time enrichment bridge** to
   trusty-analyze's class-qualified method nodes via `(file, Class.Method)`
   — explicitly not a storage relationship.

**Rejected alternatives.**
- *Ingest into trusty-analyze* (original Option A target): there is no
  durable graph store there to receive it — by design — and the FactStore is
  the wrong shape (flat triples, no traversal, full-scan reads).
- *Option B, federated MCP server, as the primary contract*: leaves the data
  outside core persistence/ops (no warm-boot, no shared traversal surface,
  every consumer composes results manually). The wire contract here is
  transport-shaped, so a federated wrapper can still be layered on later
  without changing storage.
- *A dedicated graph engine*: foreclosed by the #692 evaluation.

## Consequences

**Positive.**
- One durable home for contributed relations, surviving restart *and*
  reindex; no new storage engine, no new daemon.
- Reuses the epic's in-flight work (#815–#818) instead of inventing a
  parallel vocabulary; the contract is extractor-agnostic (SQL today;
  endpoints/queues/config producers later — `AccessesResource` is reserved
  for exactly that).
- Cross-tier queries validated on both pilot archetypes run as 2–3-hop BFS
  inside the contributed graph, with clean identity, independent of either
  daemon's (unsound) derived C# call graph.
- Self-contained identity means producer and host can evolve independently;
  the 87% analyze-side bridge is available where enrichment is wanted.

**Negative.**
- trusty-search now hosts non-code-symbol nodes: vocabulary growth, and the
  contributed overlay counts toward the per-index graph RAM budget (#705's
  axis — the binding constraint named by the #692 evaluation).
- Identity remains dual (derived bare-name symbols vs. contributed canonical
  ids); unification happens at query time or not at all. A storage-time
  merge is deliberately ruled out.
- `POST /indexes/{id}/graph` is a new public surface that must be versioned
  with the edge-kind tag set.

**Neutral / follow-ups.**
- Index-deletion housekeeping: contributed tables must be dropped with the
  index (same lifecycle gap already noted for the analyze SCIP overlay).
- The reference extractor needs an emit mode matching the wire shape
  (mechanical mapping from its native edge list).
- Future: Roslyn-resolved C#→C# call edges contributed into the same overlay
  would give sound transitive method-level traversal within the overlay,
  routing around #913 rather than waiting on it.

## References

- Discussion #580; epic #814; children #815, #816, #817, #818, #819
- #692 backing-store evaluation (graph-store tier comment, 2026-06-10)
- #913 (trusty-analyze CALLS edges unresolvable), #114 (bare-name symbol
  schema, by design), #705 (graph RAM budget), #824 (KG chunk truncation)
- `crates/trusty-search/src/core/symbol_graph.rs` (redb persistence +
  petgraph rehydration), `crates/trusty-analyze/src/core/facts.rs`
  (FactStore), `crates/trusty-analyze/src/lang/adapters/csharp.rs`
  (class-qualified node ids)
