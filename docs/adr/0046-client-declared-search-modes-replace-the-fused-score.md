# 0046. The client declares the search mode; the fused score is removed

- **Status:** Proposed
- **Date:** 2026-08-12
- **Scope:** crate `trusty-search` — the search entry point, lane selection, and
  the result envelope; consuming code in `trusty-review`, `trusty-code`, and the
  `trusty-search` Svelte UI
- **Reversibility Cost:** High — removing the fused score changes the response
  body every consumer deserializes, and restoring RRF later means restoring the
  classifier's routing role with it
- **Decision Drivers:** the owner's rejection of a calibrated per-chunk
  confidence signal ([#4976](https://github.com/bobmatnyc/trusty-tools/issues/4976));
  a search entry point that takes zero caller input and infers intent from the
  query string's lexical shape; RRF discarding the real cosine values the vector
  lane already produced; the absence of pagination anywhere on the search path
- **Supersedes / Superseded by:** —

## Context

[#4976](https://github.com/bobmatnyc/trusty-tools/issues/4976) asked for a
calibrated per-chunk relevance/confidence signal in `trusty-search`. The owner
rejected that ask. This record documents the design that takes its place; the
issue's title is superseded by it.

### The fused score exists because one endpoint serves every intent

`POST /indexes/:id/search` cannot know what its caller wanted, so it guesses.
`ChunkIndexer::search` calls `QueryClassifier::classify_with_domain` on every
query (`crates/trusty-search/src/core/indexer/search/mod.rs:149`).
`QueryClassifier::classify`
(`crates/trusty-search/src/core/classifier/classify.rs:67`) takes zero caller
input: it matches the query string against a priority-ordered regex chain, first
match wins, and returns a `QueryIntent`. `classify_with_domain`
(`classify.rs:315`) adds a second layer — an `Unknown` result is upgraded to
`Definition` when the query contains any registered domain term.

One guessed value drives the whole pipeline:

- RRF lane weights and whether the KG runs at all:
  `let (alpha, beta, use_kg_first) = intent.weights();` (`mod.rs:150`, values at
  `crates/trusty-search/src/core/classifier/intent.rs:19-28`), fed into
  `rrf_fuse` at `mod.rs:250`.
- A caller's explicit `mode: Code` is silently overridden to `All` for
  `Conceptual`, `Definition`, and `Unknown` intents (`mod.rs:164-169`). An
  explicit caller declaration is already being overruled by inference today.
- KG participation: `let effective_use_kg = use_kg_first || force_kg;`
  (`mod.rs:275`), so an inferred `Usage` intent runs a graph traversal the caller
  never asked for.
- Which edge kinds get traversed: `edge_kinds_for_intent`
  (`crates/trusty-search/src/core/indexer/search/lanes.rs:518`). No
  caller-supplied edge list exists.
- A conditional third grep lane for `Definition` intent (`mod.rs:242-247`), a
  synthetic top-1 entity injection (`inject_entity_exact_match`, `mod.rs:530`),
  doc demotion and struct boosting (`mod.rs:429-437`, applied `mod.rs:477-504`),
  and soft-versus-hard mode filtering (`mod.rs:353-354`).

Pinning `stage` today bypasses lane selection (`mod.rs:184-189`) but not the
score multipliers, because `search` classifies before it reads `stage`. The
existing per-lane MCP tools — `search_lexical`, `search_semantic`, `search_kg`
(`crates/trusty-search/src/mcp/tools/descriptors.rs:31,56,77`) — are therefore
only half-explicit.

### The score answers neither question

`rrf_fuse` is rank-only by construction. It reads each lane's score into `_` and
discards it (`crates/trusty-search/src/core/search/rrf.rs:38,42`); its own test
`test_rrf_uses_rank_not_score_magnitude` (`rrf.rs:110`) shows a BM25 score of
1000.0 and one of 0.01 tying exactly. The number the endpoint publishes is a
blend of two rank orders under weights picked by a regex guess. It is not a
relevance measure, and calibrating it would only make a blend of two different
questions look authoritative.

The owner's position on the blend: *"Why fuse them? They're different queries."*
And on the replacement: *"All should be offered as explicit options when the
client knows how they want to search."*

### Paging replaces thresholding

This is the reason no calibrated signal is needed, and it is the argument the
rest of the decision rests on.

A confidence number is only ever used to answer "should I keep reading?" A
consumer that can ask for the next page answers that question by asking. It never
needs to know whether result #7 cleared a quality bar, because the cost of being
wrong about #7 is one more request. Sorting, likewise, happens strictly within
one result set: an ordering needs only that its own members be comparable to each
other, never that a number mean the same thing across two queries or two
indexes. Remove the cross-set comparison and the calibration requirement goes
with it.

`trusty-search` has no pagination on the search path at all. `SearchQuery`
(`crates/trusty-search/src/core/indexer/types.rs:129-160`) and
`GlobalSearchRequest`
(`crates/trusty-search/src/service/server/search_global.rs:33-89`) are
`top_k`-only, as is every search tool schema in `descriptors.rs`. The one
paginated surface, `list_chunks` (`descriptors.rs:287-302`), walks the raw corpus
in file and start-line order; it does not rank and cannot serve as page 2 of a
result set.

### What is already explicit

`SearchQuery` carries `stage` (`Lexical` / `Semantic` / `Graph`, `types.rs:156`)
— the closest existing precedent for a declared mode — plus `mode`
(`types.rs:148`), `expand_graph` (`types.rs:134`, which can opt out of KG but not
in), and `refine_query` (`types.rs:160`), the one place a raw cosine sort is
already caller-triggered, scoped to post-KG neighbours
(`crates/trusty-search/src/core/indexer/search/kg.rs:115-135`).
`GlobalSearchRequest` already carries explicit fan-out: `indexes`, `routing`,
`routing_n`, `routing_threshold` (`search_global.rs:47-67`).

KG results are already additive rather than fused: `kg.rs:138` does
`all.extend(expanded)` after RRF has run. Returning KG as a distinct labelled set
is closer to today's behavior than to a rewrite.

## Decision

We will make the search mode a client declaration and remove the fused score.

1. **BM25 is the primary retrieval.** Its order is the default ordering of a
   lexical result set.
2. **BM25 and KG are both returned, paged, and un-fused**, as distinct labelled
   sets. A caller sees which question produced which results.
3. **RRF fusion is removed.** `rrf_fuse` and the single blended `score` field
   leave the search path.
4. **Cosine is exposed as a sort, never as a published score.** A caller may ask
   for a result set to be ordered by cosine similarity to the query. The
   similarity value orders that one set; it is not published as a
   cross-set-comparable number, and no threshold is applied to it.
5. **Dedicated endpoints per use case**, rather than one endpoint inferring which
   use case it is serving.
6. **A query with no declared mode returns BM25 and KG, both sets, page 1**,
   un-fused and separately labelled:

   ```
   POST /indexes/:id/search  { "text": "..." }
     -> { "lexical": [ ...page 1... ], "graph": [ ...page 1... ] }
   ```

   The caller sees what each question returned and declares from there.

7. **No calibration, no confidence value, no quality threshold, and no published
   relevance number** is added anywhere. Paging is the mechanism by which a
   consumer decides whether to keep reading.

### Three axes, kept orthogonal

| Axis | Question it answers | Surface |
|---|---|---|
| **Fan-out** | WHERE to look — widen across indexes and collections | [#2848](https://github.com/bobmatnyc/trusty-tools/issues/2848)'s staged fan-out, plus today's `GlobalSearchRequest` routing |
| **Page** | HOW MUCH — more of the same kind, within the current scope | the net-new paging mechanism |
| **Vector** | WHAT KIND — a different mode of exploration | the declared vector mode and the cosine sort |

Vector is not a fallback for "BM25 returned too few." Fan-out is what addresses
poor results. Wiring vector in as an automatic fallback would collapse a client
choice back into a daemon guess, which is the thing this decision removes.

Fan-out remains [#2848](https://github.com/bobmatnyc/trusty-tools/issues/2848)'s
own work, on its own terms. This decision references it and does not absorb,
subsume, or supersede it.

## Rejected alternatives

### Calibrating the fused score ([#4976](https://github.com/bobmatnyc/trusty-tools/issues/4976) as filed)

Rejected by the owner. Calibration would give a cross-query-comparable number to
a quantity that blends two different questions under regex-chosen weights. It
also entrenches the omnibus endpoint, because a single number is only needed when
a single ordering has to serve every caller.

### Keeping RRF and adding declared modes alongside it

Rejected. Two orderings would then exist for the same request, and the fused one
would remain the default that consumers read. The classifier would keep its
routing role to feed it.

### Vector as an automatic fallback when BM25 returns few results

Rejected. It reintroduces intent inference under a different name: the daemon
decides the caller wanted a different kind of exploration, from a result count.
Fan-out is the axis that addresses thin results; a caller who wants vector asks
for vector.

### The undeclared caller: three options, two rejected

**BM25 page 1 alone.** The narrowest reading of "BM25 is primary." Rejected: an
undeclared caller learns nothing about what KG would have returned, so it cannot
make the declaration the design asks of it. The design would require a
declaration while withholding the information needed to make one.

**400, mode required.** Maximally explicit. Rejected: it breaks every existing
consumer at once, including the UI and `trusty-review`, on the first deploy.

**Keep inference for undeclared callers only.** Rejected: it keeps the guessing
layer as a permanent second code path, which means RRF survives on that path. A
default that resurrects the fused score is the old design wearing a flag.

**Accepted: BM25 + KG, both sets, page 1.** This costs a KG traversal on every
bare query. That cost is accepted. It costs no intent inference.

## Consequences

**Easier:**

- A caller that knows what it wants says so, and gets exactly that lane's
  results in that lane's own order.
- A consumer that wants more results asks for the next page instead of
  interpreting a number.
- The KG-as-distinct-set change is small: `kg.rs:138` already extends rather than
  fuses.
- Deleting `rrf_fuse` from the search path removes the code that discards the
  vector lane's real cosine values.

**Harder:**

- **Paging is net-new and is the largest single cost.** Nothing on the search
  path pages today.
- **Five consumers read the `score` field** and must be migrated. The migration
  table is in DOC-69 §8; the only live human exposure is
  `crates/trusty-search/ui/src/lib/views/Search.svelte:266-268`, which renders
  `{(r.score ?? 0).toFixed(3)}` under a literal "score" label.
- **The response envelope changes shape**, from a flat result list to labelled
  sets. Every deserializer changes with it.
- **A bare query now always costs a KG traversal.** Accepted above.
- The classifier's fate is not settled by this record; see DOC-69 §6 and the open
  questions below.

**Neutral:**

- `crates/trusty-agents` has a separate, independently owned search stack with
  its own RRF and a `grade_from_score` A–F letter grade on fixed thresholds
  (`crates/trusty-agents/src/tools/native_search/helpers.rs:103-115`). This
  decision does not reach it.

## Open questions

- **Does the classifier survive at all, and in what role?** Removing its routing
  and scoring authority leaves it with no consumer on the search path. Whether it
  is deleted, kept as an optional caller-requested hint, or kept for the
  non-search surfaces is undecided.
- **Cursor or offset for paging**, and what a stable page boundary means over an
  index being written to concurrently. DOC-69 §4 states the constraints; it does
  not pick.
- **Does the cosine sort accept re-embedding cost, or refuse to sort chunks it
  cannot score?** DOC-69 §5 quantifies the cost. The behavior when a chunk's
  vector is unavailable is undecided.
- **Whether the per-lane MCP tools become the dedicated endpoints**, or whether
  the dedicated endpoints are new and the MCP tools become thin wrappers.

## Related Decisions

Vetted against `docs/adr/INDEX.md` on 2026-08-12:

- **ADR-0038 (KG stays additive, recall gated on extraction quality):**
  **Extends.** ADR-0038 keeps the KG additive rather than primary in recall. This
  decision keeps KG additive and makes the additivity visible in the response by
  returning it as its own labelled set instead of merging it into one list. It
  does not promote KG to a primary lane.
- **ADR-0018 (Loopback-only doctrine):** **No interaction.** The endpoint surface
  changes shape; its bind address and trust boundary do not.
- **ADR-0032 (Console is the only HTTP surface), ADR-0031 (UDS between crates):**
  **Consistent.** This decision changes what a search request and response carry,
  not which process listens or on what transport.
- **ADR-0001 (Design docs live in top-level `docs/`):** **Consistent.** This
  record and DOC-69 live in `docs/adr/` and `docs/specs/`.

No prior Accepted decision contradicts this one.
