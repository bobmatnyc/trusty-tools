---
spec_refs: []
---

# DOC-69 — Client-Declared Search Modes: Un-Fused Lanes, Paging, and the Cosine Sort

**Status:** Draft
**Spec ID:** `SPEC-DECLSEARCH-01~draft` … `SPEC-DECLSEARCH-09~draft`
**Subsystem:** `trusty-search` — the search entry point (`ChunkIndexer::search`),
lane selection, the HTTP and MCP search surfaces, and the result envelope.
Consumed by `trusty-review` (DD context assembly), `trusty-code` (search
telemetry), and the `trusty-search` Svelte UI
**Owner:** Bob Matsuoka
**Last-updated:** 2026-08-12
**DOC-N claim:** `DOC-69`, scan-before-claim per
[DOC-38 §4.1](./spec-linked-documentation.md). Verified in a worktree branched
from `origin/main` (`c40d8f3ce`): no `DOC-69` filename or header self-label
anywhere under `docs/specs/**`; `docs/specs/README.md`'s catalog note names
`DOC-69` as the next free number after `DOC-68`; `scripts/check_doc_numbers.sh`
reported 118 docs / 112 claims, 3 grandfathered, 0 violations before this file
was added; a `gh pr list` sweep of open pull requests found no `DOC-69` or
`DOC-70` claim.
**Builds on:** [ADR-0046](../adr/0046-client-declared-search-modes-replace-the-fused-score.md)
— the decision this spec implements. Read it first; this document does not
restate its reasoning.
**Cross-ref:** [#4976](https://github.com/bobmatnyc/trusty-tools/issues/4976)
(the calibrated-confidence ask this design replaces, rejected by the owner);
[#2848](https://github.com/bobmatnyc/trusty-tools/issues/2848) (staged fan-out —
orthogonal, open on its own terms, not subsumed by this spec);
[ADR-0038](../adr/0038-kg-stays-additive-recall-gated-on-extraction-quality.md)
(KG stays additive)

---

## 1. Scope and what this spec replaces {#SPEC-DECLSEARCH-01~draft}

**ID:** SPEC-DECLSEARCH-01~draft
**Status:** Draft

This spec defines the search surface after
[ADR-0046](../adr/0046-client-declared-search-modes-replace-the-fused-score.md):
the client declares how it wants to search, the daemon stops inferring it, the
RRF-fused score is removed, and BM25 and KG results are returned as separate
labelled sets.

**In scope:** the endpoint and parameter surface (§2), the undeclared-caller
response (§3), paging (§4), the cosine sort (§5), the classifier's fate (§6),
the boundary with fan-out (§7), and consumer migration (§8).

**Out of scope, stated so nobody adds it back:**

- Any calibrated relevance signal, confidence value, quality threshold, or
  published per-chunk relevance number. [#4976](https://github.com/bobmatnyc/trusty-tools/issues/4976)
  asked for one and the owner rejected it. §5 states why the design does not need
  one.
- Fan-out design. [#2848](https://github.com/bobmatnyc/trusty-tools/issues/2848)
  owns it (§7).
- `crates/trusty-agents`, which has its own independently owned search stack with
  its own RRF and an A–F letter grade on fixed thresholds
  (`crates/trusty-agents/src/tools/native_search/helpers.rs:103-115`). Different
  crate, different stack, not touched by this work.

### 1.1 A sort, not a score

The one sentence that governs every section below: **sorting happens within one
result set, so nothing needs to be comparable across sets.** A cosine value
orders the members of a single response against each other. It is never published
as a number a caller compares to another query's number, and it is never compared
to a threshold.

**Paging replaces thresholding.** A consumer that can ask for the next page never
needs to know whether result #7 cleared a quality bar — the cost of being wrong
about #7 is one more request. That is the structural reason no calibrated signal
is required.

---

## 2. The endpoint and parameter surface {#SPEC-DECLSEARCH-02~draft}

**ID:** SPEC-DECLSEARCH-02~draft
**Status:** Draft

### 2.1 What is explicit today

The surface is not being invented from nothing. `SearchQuery`
(`crates/trusty-search/src/core/indexer/types.rs:129-160`) already carries:

| Field | Line | What it declares |
|---|---|---|
| `stage` | `types.rs:156` | `Lexical` / `Semantic` / `Graph` — the closest existing precedent for a declared mode |
| `mode` | `types.rs:148` | `Code` / `Text` / `Data` content-kind filter |
| `expand_graph` | `types.rs:134` | opts OUT of KG expansion; there is no way to opt IN |
| `refine_query` | `types.rs:160` | the one place a raw cosine sort is already caller-triggered, scoped to post-KG neighbours (`core/indexer/search/kg.rs:115-135`) |
| `top_k` | `types.rs:132` | result count — the only sizing control anywhere |

`GlobalSearchRequest`
(`crates/trusty-search/src/service/server/search_global.rs:33-89`) already
carries explicit fan-out: `indexes` (`:47`), `routing` (`:61`), `routing_n`
(`:64`), `routing_threshold` (`:67`).

The MCP surface already exposes per-lane tools: `search_lexical`,
`search_semantic`, `search_kg`, and the omnibus `search_all` and `search`
(`crates/trusty-search/src/mcp/tools/descriptors.rs:31,56,77,98,126`).

### 2.2 Why the existing explicit surface is only half-explicit

Pinning `stage` selects lanes (`core/indexer/search/mod.rs:184-189`) but does not
escape intent inference. `ChunkIndexer::search` calls `classify_with_domain`
before it ever reads `stage` (`mod.rs:149`), and the resulting `QueryIntent`
still drives:

- doc demotion and struct boosting inside `apply_score_adjustments`
  (`mod.rs:429-437`, applied `mod.rs:477-504`),
- the synthetic top-1 entity injection (`inject_entity_exact_match`,
  `mod.rs:530-546`),
- soft-versus-hard mode filtering (`mod.rs:353-354`),
- and — the sharpest case — a silent override of the caller's own declaration:
  an explicit `mode: Code` becomes `All` for `Conceptual`, `Definition`, and
  `Unknown` intents (`mod.rs:164-169`).

A design whose premise is "the client declares it" cannot leave a path where an
explicit declaration is overruled by a regex match on the query text.

### 2.3 The declared surface

Every request carries the mode as data, not as an inference target. The
normative requirements:

- **R2.1** A caller MUST be able to declare which lanes run, and MUST get exactly
  those lanes. No lane runs that the caller did not declare, except in the
  undeclared case (§3).
- **R2.2** A caller's declaration MUST NOT be overridden by any property of the
  query text. `mod.rs:164-169`'s override is removed.
- **R2.3** The response MUST label each result set by the lane that produced it.
  Results from different lanes are never merged into one ordering.
- **R2.4** No response field carries a fused, blended, or cross-lane score.
- **R2.5** Every result set MUST be pageable (§4).
- **R2.6** KG traversal MUST be opt-in by declaration, not by inferred intent.
  `effective_use_kg = use_kg_first || force_kg` (`mod.rs:275`) loses its
  `use_kg_first` term.
- **R2.7** A caller SHOULD be able to declare which KG edge kinds to traverse.
  Today `edge_kinds_for_intent` (`core/indexer/search/lanes.rs:518-539`) derives
  them from the guessed intent and no caller-supplied list exists.

**Dedicated endpoints per use case** replace the omnibus endpoint's guessing.
Whether the existing per-lane MCP tools (§2.1) become those endpoints, or whether
new endpoints are added and the MCP tools become thin wrappers over them, is
undecided — see §9.

---

## 3. The undeclared caller {#SPEC-DECLSEARCH-03~draft}

**ID:** SPEC-DECLSEARCH-03~draft
**Status:** Draft

A query with no declared mode returns **BM25 and KG, both sets, page 1**,
un-fused and separately labelled:

```
POST /indexes/:id/search  { "text": "..." }
  -> { "lexical": [ ...page 1... ], "graph": [ ...page 1... ] }
```

- **R3.1** The undeclared response MUST return both sets, separately labelled.
- **R3.2** Neither set is fused with the other, and no field ranks a member of one
  against a member of the other.
- **R3.3** BM25 is the primary retrieval; the `lexical` set is in BM25's own
  order.
- **R3.4** The undeclared path runs no intent classification.

The caller sees what each question returned and declares from there.

**Accepted cost:** every bare query pays for a KG traversal. That is the price of
returning something a caller can act on without the daemon guessing, and it is
accepted.

**Rejected alternatives** are recorded in
[ADR-0046](../adr/0046-client-declared-search-modes-replace-the-fused-score.md)
("The undeclared caller: three options, two rejected"): BM25 page 1 alone
(leaves the caller unable to make the declaration the design asks for), 400 with
mode required (breaks every consumer at once), and keeping inference for
undeclared callers only (keeps the guessing layer, and RRF with it, as a
permanent second code path).

---

## 4. Paging {#SPEC-DECLSEARCH-04~draft}

**ID:** SPEC-DECLSEARCH-04~draft
**Status:** Draft

### 4.1 Nothing on the search path pages today

`SearchQuery` and `GlobalSearchRequest` are `top_k`-only, and every search tool
schema in `descriptors.rs` declares `top_k` and nothing else
(`descriptors.rs:39,64,85,106,134,231,375`).

The one paginated surface is `list_chunks` / `GET /indexes/:id/chunks`
(`descriptors.rs:287-302`), which offers both `offset`/`limit` and an `after`
cursor. It walks the raw corpus in file and start-line order. It does not rank,
so it cannot serve as page 2 of a ranked result set.

**Paging is net-new work and is the largest single cost in this design.** It is a
mechanism, not a parameter.

### 4.2 Requirements

- **R4.1** Every labelled result set is independently pageable. Advancing the
  `lexical` set does not advance the `graph` set.
- **R4.2** A page request MUST be answerable without re-running the whole query
  from scratch, or the design has traded one cost for a worse one.
- **R4.3** Page boundaries MUST NOT drop or duplicate a result that was present
  and unchanged in the index across the two requests.
- **R4.4** A caller MUST be able to distinguish "no more results" from "more
  results exist."

### 4.3 Cursor versus offset — undecided, with the constraints stated

Both are viable and the choice is not made here.

| | Offset | Cursor |
|---|---|---|
| Client complexity | lowest — an integer | opaque token round-tripped |
| Cost of deep pages | re-ranks and discards `offset` results per request | can resume from a position |
| Behavior under concurrent writes | shifts: an insert above the window duplicates a result, a delete skips one | can pin a snapshot, at the cost of holding one |
| Precedent in this crate | `list_chunks` `offset`/`limit` (`descriptors.rs:300-301`) | `list_chunks` `after` (`descriptors.rs:302`), added because the offset scan times out on large indexes |

The `list_chunks` precedent is informative: the crate already found that an
offset scan does not survive large indexes and added a cursor beside it.
Whether ranked paging inherits that conclusion depends on where the page
boundary is materialized, which §4.4 has not settled.

### 4.4 What a stable page boundary means over an index being written to

This is the hard part and it is genuinely open.

`trusty-search` indexes are written concurrently with reads: the filesystem
watcher ingests changes while queries run. A ranked page boundary over a moving
corpus admits several definitions, each with a different cost:

1. **No stability guarantee.** Page 2 is whatever ranks 11–20 at the moment page
   2 is requested. Cheapest. A file indexed between the two requests can push a
   result from page 1 onto page 2, so the caller sees it twice; a deletion can
   make a result vanish unseen.
2. **Snapshot per result set.** The lane's candidate list is materialized once
   and held for a bounded lifetime; pages are served from it. Stable within the
   snapshot's lifetime. Costs memory proportional to the materialized candidate
   count per live paging session, and needs an eviction policy and a defined
   behavior when a caller pages after expiry.
3. **Cursor over a stable secondary key.** Pages resume from `(score, chunk_id)`
   or similar. Avoids holding state. Requires that the ordering key be stable
   under concurrent writes, which BM25 scores are not — a corpus change moves
   IDF and therefore every score.

Option 3's problem is specific and worth naming: BM25 scores depend on
corpus-wide term statistics, so an ingest between page 1 and page 2 changes the
scores of documents that did not themselves change. A cursor keyed on score is
resuming from a value that no longer means what it meant.

- **R4.5** The chosen definition MUST be stated in the response contract, so a
  consumer knows whether it may rely on page stability. An unstated guarantee is
  the failure mode here, not a weak one.

Which of the three this design adopts is **undecided** (§9).

---

## 5. The cosine sort {#SPEC-DECLSEARCH-05~draft}

**ID:** SPEC-DECLSEARCH-05~draft
**Status:** Draft

Cosine similarity is exposed as an **action a caller takes on a result set** —
"sort these by similarity to my query" — for expanding results. It is a sort
within one set. It is never a score published across sets, never compared to a
threshold, and never calibrated.

### 5.1 The value survives fusion, and dies one step later

`hnsw_results: Vec<(String, f32)>` (`core/indexer/search/mod.rs:227-230`) holds
real per-chunk cosine similarity in "higher is better" form, `1 − cos_dist`
(`core/indexer/search/lanes.rs:444-460`).

`rrf_fuse` does not consume it: it reads the score into `_` and uses rank alone
(`core/search/rrf.rs:38,42`). Its own test
`test_rrf_uses_rank_not_score_magnitude` (`rrf.rs:110`) shows a BM25 score of
1000.0 and one of 0.01 tying exactly.

But `hnsw_results` stays in scope and reaches `materialize_search_results`
(`mod.rs:302-309`), where `materialize.rs:40` builds
`let in_hnsw: HashSet<&String> = hnsw_results.iter().map(|(id, _)| id).collect();`
and uses membership only, discarding the `f32`.

**The cosine value dies at a `HashSet` in `materialize.rs:40`, not at
`rrf_fuse`.** A `HashMap` there would keep it. For chunks the vector lane
returned, exposing a cosine sort is close to free.

### 5.2 The hard constraint: no durable per-chunk vector store

The cosine value exists only for chunks HNSW's own traversal returned for this
query — `let want = query.top_k.saturating_mul(HNSW_OVERSAMPLE).max(query.top_k);`
(`mod.rs:198`).

There is no way to reconstruct a chunk's vector from the store. `VectorStore`
(`crates/trusty-search/src/core/store/types.rs:54-95`) exposes `upsert`,
`search`, `remove`, `len`, and `search_filtered`. There is no `get` and no
reconstruction path.

The only other source is a bounded LRU. `ChunkIndexer::get_embedding`
(`core/indexer/search/lanes.rs:138-144`) peeks `chunk_embeddings`, an
`LruCache<String, Vec<f32>>` (`core/indexer/mod.rs:177,544`) whose default
capacity is **1 000 entries per index**
(`DEFAULT_EMBEDDING_CACHE_CAP`, `core/indexer/helpers.rs:36`; ~1.5 MB at 384-dim
f32, lowered from 10 000 under issue #79 after a daemon was observed at 43.9 GB
RSS, overridable via `TRUSTY_EMBEDDING_CACHE`). It returns `None` for an evicted
chunk and for any index built in BM25-only mode.

**Therefore: cosine-sorting an arbitrary BM25 result set requires re-embedding
those chunks.** On an index of 200 000 chunks, a 1 000-entry cache covers 0.5% of
the corpus; the expected hit rate for an arbitrary BM25 page is low, and the
common case is a cache miss.

A second, smaller cost applies on the lexical-only path: `embedding` is `None`
when `stage` is `Lexical` (`mod.rs:191-196`), so sorting a lexical page by cosine
also requires embedding the query itself.

### 5.3 Requirements

- **R5.1** The cosine sort is a caller-declared action. It never fires on its own.
- **R5.2** The sort orders one labelled result set. It never reorders across sets
  and never merges them.
- **R5.3** No similarity value is published as a cross-set-comparable score, and
  none is compared to a threshold.
- **R5.4** The re-embedding cost MUST be visible to the caller before it is paid —
  by an explicit opt-in, a documented cost, or both. A sort that silently embeds a
  page of chunks is a latency trap.

### 5.4 Undecided: what happens to a chunk with no vector

When a chunk's embedding is neither in `hnsw_results` nor in the LRU, the design
has three options and picks none here: re-embed it (correct ordering, pays the
cost), omit it from the sorted set (cheap, silently drops results), or return it
unsorted at the end (keeps every result, produces a partial ordering the caller
must understand). See §9.

**Vector is not a fallback for "BM25 returned too few."** Fan-out (§7) is what
addresses poor results. Auto-triggering a vector lane on a thin result count
would collapse a client choice back into daemon guessing, which is exactly what
this design removes.

---

## 6. What happens to the classifier {#SPEC-DECLSEARCH-06~draft}

**ID:** SPEC-DECLSEARCH-06~draft
**Status:** Draft

### 6.1 What it is

`QueryClassifier::classify`
(`crates/trusty-search/src/core/classifier/classify.rs:67`) takes **zero caller
input**. It matches the query string's lexical shape against a priority-ordered
regex chain, first match wins, and yields a `QueryIntent`.
`classify_with_domain` (`classify.rs:315`) adds a second layer: an `Unknown`
result is silently upgraded to `Definition` when the query contains any
registered domain term.

### 6.2 Everything it currently feeds

| Consumer of the guessed intent | Site | Fate under this design |
|---|---|---|
| RRF lane weights `(alpha, beta, use_kg_first)` | `mod.rs:150`, values `classifier/intent.rs:19-28` | removed with RRF |
| Explicit `mode: Code` → `All` override | `mod.rs:164-169` | removed (R2.2) |
| KG participation `use_kg_first \|\| force_kg` | `mod.rs:275` | `use_kg_first` term removed (R2.6) |
| Edge kinds traversed | `lanes.rs:518-539` | caller-declared (R2.7) |
| Conditional third grep lane (`Definition` only) | `mod.rs:242-247` | becomes a declared lane or is dropped — undecided |
| Synthetic top-1 entity injection | `mod.rs:530-546` | undecided |
| Doc demotion + struct boosting | `mod.rs:429-437`, applied `:477-504` | undecided |
| Soft-vs-hard mode filtering | `mod.rs:353-354` | undecided |

- **R6.1** No caller-visible behavior is selected by inferred intent. Every row
  above is either removed, or converted into something the caller declares.
- **R6.2** The classifier MUST NOT run on a path where the caller declared its
  mode.

### 6.3 Undecided: does the classifier survive

Once its routing and scoring authority is gone, the classifier has no consumer on
the search path. Three possibilities, none chosen here: delete it; keep it as an
optional hint the caller explicitly asks for and may ignore; or keep it for
non-search surfaces (`typeahead`, `chat`) that this spec does not cover. See §9.

---

## 7. Fan-out is a separate axis and separate work {#SPEC-DECLSEARCH-07~draft}

**ID:** SPEC-DECLSEARCH-07~draft
**Status:** Draft

Three axes, kept orthogonal:

| Axis | Question | Owned by |
|---|---|---|
| **Fan-out** | WHERE to look — widen across indexes and collections | [#2848](https://github.com/bobmatnyc/trusty-tools/issues/2848), plus today's `GlobalSearchRequest` routing (`search_global.rs:47-67`) |
| **Page** | HOW MUCH — more of the same kind, current scope | §4 of this spec |
| **Vector** | WHAT KIND — a different mode of exploration | §5 of this spec |

- **R7.1** Poor or thin results are addressed by fan-out, never by automatically
  switching lanes.
- **R7.2** This spec does not define, subsume, or supersede staged fan-out.
  [#2848](https://github.com/bobmatnyc/trusty-tools/issues/2848) stays open on its
  own terms.

Collapsing any two of these axes puts intent inference back: a design where "few
results" implies "try vector" is the daemon deciding what the caller wanted.

---

## 8. Consumer migration {#SPEC-DECLSEARCH-08~draft}

**ID:** SPEC-DECLSEARCH-08~draft
**Status:** Draft

Every workspace consumer of the fused `score`, from an exhaustive grep. This is
the full migration surface.

| Consumer | Site | What it does with the score | Migration |
|---|---|---|---|
| trusty-search UI | `crates/trusty-search/ui/src/lib/views/Search.svelte:266-268` | renders `{(r.score ?? 0).toFixed(3)}` under a literal `score` label to a human — **the only live human exposure** | Remove the score display. The lane label replaces it as the thing the reader needs. Highest-visibility change in this table. |
| trusty-review | `crates/trusty-review/src/integrations/search_client.rs:107-115` | deserializes it into `SearchResult.score`, doc-commented "Combined relevance score" | Drop the field, or repoint it at the lane label. The doc comment is a factual claim that stops being true. |
| trusty-review | `crates/trusty-review/src/integrations/apex_context.rs:127` | copies it into `ApexContextResult.score` | Follows `search_client.rs`. Mechanical. |
| trusty-review | `crates/trusty-review/src/pipeline/prompt_user_msg.rs` | **never reads it** — takes the top 10 on faith (`grep score` over the file returns nothing) | No change. Already dead as prompt content, which is evidence the score was not doing the job attributed to it. |
| trusty-code | `crates/trusty-code/src/tools/trusty_search.rs:408-451` | parses it for telemetry only — no sort, no threshold (`SearchHit { path, score }`, `:447`) | Drop the field from `SearchHit`, or record the lane instead. No behavior depends on the value. |

**R8.1** No consumer sorts by, thresholds on, or branches on the fused score
today. The only behavior that changes for a human is the UI's rendered number.

**R8.2** The response envelope changes shape from a flat list to labelled sets;
each row above changes its deserializer regardless of whether it kept reading the
score.

---

## 9. Open questions {#SPEC-DECLSEARCH-09~draft}

**ID:** SPEC-DECLSEARCH-09~draft
**Status:** Draft

Genuinely undecided. Each needs a ruling before implementation.

- **Q1 — Cursor or offset for paging** (§4.3). The `list_chunks` precedent favors
  a cursor for large indexes; whether ranked paging inherits that depends on Q2.
- **Q2 — What a stable page boundary means over an index being written to**
  (§4.4). Three definitions, each with a different cost. Naming the specific
  obstacle: BM25 scores depend on corpus-wide term statistics, so an ingest
  between two page requests changes the scores of documents that did not change,
  which breaks a cursor keyed on score.
- **Q3 — What the cosine sort does with a chunk it cannot score** (§5.4):
  re-embed, omit, or append unsorted.
- **Q4 — Does the classifier survive, and in what role** (§6.3): delete, optional
  caller-requested hint, or retained for non-search surfaces only.
- **Q5 — Do the per-lane MCP tools become the dedicated endpoints**, or are new
  endpoints added with the MCP tools as thin wrappers (§2.3)?
- **Q6 — Which intent-driven adjustments become declarations and which are
  dropped** (§6.2, the four "undecided" rows): the grep lane, the entity
  injection, doc demotion and struct boosting, and soft-vs-hard mode filtering.
  Each was added for a specific issue and each needs its own call.
- **Q7 — Default page size**, and whether it differs per lane. `top_k` defaults to
  10 across the MCP schemas; whether a page inherits that is unstated.
- **Q8 — Does the undeclared response page the two sets in lockstep** or expose
  two independent cursors (§3, §4.2)? R4.1 says independent; the undeclared
  envelope has not been checked against that.

---

## 10. Change log

- **2026-08-12** — Initial draft (DOC-69, `SPEC-DECLSEARCH-01~draft` …
  `SPEC-DECLSEARCH-09~draft`), recording the design that replaces
  [#4976](https://github.com/bobmatnyc/trusty-tools/issues/4976)'s rejected
  calibrated-confidence ask, per
  [ADR-0046](../adr/0046-client-declared-search-modes-replace-the-fused-score.md).
