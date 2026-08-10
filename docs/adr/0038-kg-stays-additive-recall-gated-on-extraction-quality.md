# 0038. The knowledge graph stays additive in recall; a KG-primary lane is gated on three extraction-quality preconditions

- **Status:** Accepted
- **Date:** 2026-08-10
- **Scope:** crates `trusty-common` (`memory_core`), `trusty-memory`
- **Reversibility Cost:** Low for the decision itself — it changes nothing in
  code today, only what may be built next. The reason this warrants an ADR is
  that the REJECTED alternative (a KG-primary recall lane) carries a High-cost
  schema migration across 94 palaces, and that cost is what this ADR spends
  its evidence establishing should not be paid yet.
- **Decision Drivers:** recall content quality under a fixed injection budget,
  extraction precision, absence of external evidence either way, migration
  cost of the KG storage key
- **Supersedes / Superseded by:** —

---

## Context

### What triggered it

[#5036](https://github.com/bobmatnyc/trusty-tools/issues/5036) reports that
the HTTP recall path is vector-only — RRF BM25 fusion is implemented but never
wired into the `UserPromptSubmit` hook. Reviewing that gap, the owner asked
whether a knowledge-graph lane would serve prompt injection better than
lexical+vector, citing the Kuzu blog post "Why Knowledge Graphs Are Critical
to Agent Context" and mem0. The owner then narrowed the question: BM25 stays
in the design regardless; the open question is whether KG-derived CONTENT
produces a better prompt-injection block than prose/drawer content does.

### External survey (2026-08-10)

No surveyed system uses a knowledge graph as a primary or standalone recall
index — the graph is additive everywhere it appears. Kuzu's own pattern is
graph-traversal-constrains-neighborhood, then vector search runs inside the
constrained set. Graphiti/Zep runs three parallel lanes (vector, BM25, graph
traversal) merged by reciprocal rank fusion — additive by construction, not a
KG-primary design.

Two capabilities survive the survey as genuinely KG-only, not just a reframed
vector feature: multi-hop relational traversal, and explicit temporal
validity. Graphiti's bi-temporal edges (`t_valid` / `t_invalid`,
invalidate-never-delete) are structurally the same principle as ADR-0028's
"demote, never delete" (§D6 there). Cost is paid at write time — Graphiti
spends at least two LLM calls per edge (a date-extraction prompt and an
invalidation prompt); retrieval itself runs no LLM, which is what keeps it
fast. Published latency comparisons are unusable as evidence: mem0 and Zep
each re-ran the other's system and reported numbers disagreeing by up to 4x,
with Zep alleging misconfiguration on mem0's side. No trustworthy third-party
head-to-head exists between these designs.

### mem0, from source (github.com/mem0ai/mem0, commit `4debc58a`, 2026-08-07)

The open-source package has **no graph store** — no `graph_memory.py`, no
`GraphMemory` class, no `graph_store` wiring outside test mocks and stale
docstrings. The graph-memory documentation under `docs/platform/` describes
the closed-source hosted Platform, not the package most people run. That
Platform's own migration note records that its external-graph-database
integration (Neo4j, Memgraph, Kuzu, Apache AGE, Neptune) was **replaced**.

What replaced it emits no injectable content at all: `_compute_entity_boosts()`
(`mem0/memory/main.py:1728-1772`) returns a numeric boost dict;
`score_and_rank()` (`mem0/utils/scoring.py:57-101`) computes
`(semantic + bm25 + entity_boost) / max_possible` with
`ENTITY_BOOST_WEIGHT = 0.5` against a max of 2.5 — the graph is capped at 20%
of ranking weight and never contributes a token to the injected block. mem0's
own docs state the mechanism plainly: connections are inferred from
co-occurrence, not declared as typed relationships. mem0 also **removed** its
two-pass ADD/UPDATE/DELETE reconciliation in favor of single-pass ADD-only,
trading consistency for latency — a second data point that the direction of
travel in this space is toward less graph structure, not more.

### No evidence either way

mem0's published evals (LOCOMO, LongMemEval, BEAM, the token-efficiency blog
post) all vary the retrieval ALGORITHM while holding "feed the model whatever
came back" constant. None holds token budget fixed and varies CONTENT TYPE
(triple vs. prose) as the independent variable. Stated plainly: the evidence
that "KG content beats lexical+vector content in a budgeted injection block"
does not exist on either side, in anything surveyed here.

### Our own state — the KG is already in the injection path, and already narrowed

`gather_hot_triples` in `crates/trusty-memory/src/prompt_facts.rs:54-59,170-200`
injects a standing-facts block filtered to four predicates: `is_alias_for`,
`has_convention`, `is_fact`, `is_shorthand_for`.
[ADR-0028](0028-memory-recall-tiers-standing-current-episodic.md) §C10
measured 32,622 triples actually injected across the corpus: 80.4% `tags`,
6.9% `mentioned-in`, 6.2% `contains` — 93.5% structural graph plumbing versus
~0.25% curated hot-predicate facts. ADR-0028 responded by **excluding** the
low-precision extraction from injection rather than repairing it, and said so
explicitly (its "What this does not fix" section).

Outside that four-predicate allowlist, the KG touches nothing that ranks.
`handle_memory_recall` (`crates/trusty-memory/src/tools/memory_ops.rs:307`)
joins vector recall with optional BM25 and fuses by RRF — no KG call.
`recall()` / `recall_deep()`
(`crates/trusty-common/src/memory_core/retrieval/layers.rs:370-453`) are
vector + importance only.

The corpus is not empty: 12,743 triples against 1,428 drawers in the
`trusty-tools` palace, up from 9,205 on 2026-08-04. Composition, not volume,
is the problem. Triples come from three sources: a regex/pattern-table
extractor that runs on every write (`crates/trusty-memory/src/kg_extract.rs`,
`provenance = "auto:remember"`, `confidence = 0.6`), which produces the 93.5%
structural share; the dream cycle's LLM consolidation, which writes
`superseded_by` / `alias_of` edges; and manual `kg_assert` calls. There is no
general-purpose semantic fact extractor in the workspace today.

Three open defects block a recall lane specifically:

- [#4810](https://github.com/bobmatnyc/trusty-tools/issues/4810) — the redb
  key is `(subject, predicate)` only, so `room:General --contains--> drawer:X`
  collapses to one triple regardless of member count, and the graph cannot
  enumerate membership of anything.
- [#4775](https://github.com/bobmatnyc/trusty-tools/issues/4775) and
  [#4776](https://github.com/bobmatnyc/trusty-tools/issues/4776) — a
  `kg_query` miss is indistinguishable from a wrong subject guess, and there
  is no subject-listing tool.
- [#4678](https://github.com/bobmatnyc/trusty-tools/issues/4678) — the
  extractor emits stopwords as first-class entities (`the` at degree 23, `a`
  at degree 12).

Epic [#1119](https://github.com/bobmatnyc/trusty-tools/issues/1119) already
rejected LLM-per-write extraction, citing the `mcp-fact-finder` post-mortem: a
15-hour index build, 41 GB of storage, $0.98 per 10K documents. Nothing has
been implemented against that epic since.

Note also that trusty-agents' "OKG" is a structurally unrelated system — a
document/entity ingestion engine in `crates/trusty-kb/src/okg/`, with no
triples and no `kg.db`. It shares a name with the memory-core KG discussed
here and nothing else; this ADR does not propose consolidating them.

---

## Decision

We will keep ranked recall on fused lexical + vector retrieval, and the KG
stays **additive** — never a primary or standalone recall lane. We will not
build KG-derived content into the injection block until three preconditions
hold, in this order:

1. **`(subject, predicate, object)` keying**, so membership is enumerable —
   [#4810](https://github.com/bobmatnyc/trusty-tools/issues/4810). This is the
   migration-costly precondition; it must land before anything relational is
   attempted, because every downstream capability depends on triples not
   collapsing.
2. **Honest introspection** — a `kg_query` miss distinguishable from a
   wrong-subject guess, plus subject enumeration —
   [#4775](https://github.com/bobmatnyc/trusty-tools/issues/4775),
   [#4776](https://github.com/bobmatnyc/trusty-tools/issues/4776).
3. **Extraction precision** good enough that entities are entities —
   [#4678](https://github.com/bobmatnyc/trusty-tools/issues/4678).

Until all three hold, any A/B test of KG content against prose content
measures our regex extractor, not the KG hypothesis — the result would be
uninterpretable regardless of which way it came out.

When the preconditions do hold, the first experiment run is the **cheap**
one: mem0-style entity-boost re-ranking, which needs no typed edges and no LLM
extraction. Its measured result gates whether the expensive Graphiti-style
typed-edge path — bi-temporal validity, multi-hop traversal, LLM-per-edge
extraction cost — is worth funding at all.

[#5036](https://github.com/bobmatnyc/trusty-tools/issues/5036) proceeds
independently on its own merits: it is a wiring gap on an already-tested
fusion function, and none of the above blocks it.

---

## Consequences

**What this defers, and its cost.** Any KG-content-in-injection work is
deferred until three issues land, in order. The cost of deferring is real:
if KG content genuinely would improve recall quality, that improvement stays
unrealized for as long as the preconditions take. That risk is accepted
because building the content path first would mean building it against a
storage key that cannot enumerate membership and an extractor that emits
stopwords as entities — work that would likely need redoing regardless of
what the A/B test showed.

**Tier S is unaffected.** ADR-0028's hot-predicate standing-facts surface
(`is_alias_for`, `has_convention`, `is_fact`, `is_shorthand_for`) keeps working
exactly as it does today; this ADR does not touch it, narrow it, or gate it
behind the three preconditions. It is a curated four-predicate allowlist, not
a general KG-recall lane.

**The three preconditions are now an ordered dependency chain, not an
unordered bug list.** Before this ADR, #4810/#4775/#4776/#4678 were four
independent open issues with no stated relationship. This ADR states plainly
that #4810 must land first — keying determines whether the other two are even
testable against real relational data — which changes how they should be
triaged and scheduled relative to each other.

**The 94-palace migration in precondition 1 is the single largest cost, and
it is paid before any measurable benefit arrives.** Re-keying every triple
store from `(subject, predicate)` to `(subject, predicate, object)` touches
every palace in the estate. That cost lands whether or not the eventual
recall experiment shows KG content helps — a real risk this ADR accepts
explicitly rather than hiding it behind "next steps."

**Choosing the cheap experiment first may under-sell typed edges.** The
strongest published case for a KG contributing to recall quality is
Graphiti's, and it is a case for exactly the typed-edge, bi-temporal design
that the cheap entity-boost experiment does not test. A cheap experiment that
returns "no measurable benefit" would not refute the Graphiti-shaped
hypothesis — it would only refute the cheaper one. That gap is accepted
because the cheap experiment is what precondition-clearing time actually
buys; funding the expensive path first, with no evidence at all, is the thing
this ADR is declining to do.

**We are deciding under absent evidence, not contrary evidence.** No survey
result found here says a KG-primary or KG-content-injected lane performs
worse than lexical+vector — the finding is that no trustworthy comparison
exists in either direction. That is a weaker basis than a decision made
against contrary evidence, and it should be treated as such: a genuinely new,
trustworthy external result — a head-to-head that holds token budget fixed
and varies content type — should reopen this ADR rather than be dismissed as
already-settled.

---

## Related Decisions

Vetted against `docs/adr/INDEX.md` on 2026-08-10:

- **[ADR-0028](0028-memory-recall-tiers-standing-current-episodic.md) (Memory
  recall splits into three tiers — Standing, Current, Episodic):** *Extends.*
  This ADR is the direct continuation of ADR-0028 §C10's finding that 93.5% of
  injected KG triples are structural plumbing, and of its decision to exclude
  that plumbing from injection rather than repair it. ADR-0028 stopped at "not
  now, not repaired here"; this ADR states the ordered precondition chain that
  decides *when* KG content injection becomes worth attempting, and holds
  ADR-0028's Tier S hot-predicate surface untouched.
- **[ADR-0027](0027-rooms-are-real-wings-are-scopes-closets-are-an-index.md)
  (Rooms are real; Wings are scopes; Closets are an index):** *Consistent.*
  Both ADRs share the same `kg.db` store, and
  [#4810](https://github.com/bobmatnyc/trusty-tools/issues/4810)'s
  `(subject, predicate, object)` re-keying touches the same redb file
  ADR-0027's new `ROOMS` table lives in. Neither ADR's migration rewrites or
  deletes an existing row — ADR-0027 adds a table, #4810's fix adds a key
  component — so the two migrations compose without ordering constraints
  between them, the same additive-only doctrine ADR-0027 established for the
  93-palace estate.
- **[ADR-0004](0004-three-harnesses-shared-event-driven-common.md) (Three
  harnesses on a shared event-driven trusty-common foundation):** *Consistent.*
  ADR-0004 establishes that `trusty-memory` is KG-backed at the foundation
  layer; this ADR does not change that foundation, only how much of it is
  exposed at recall/injection time.
- **[ADR-0009](0009-external-extractor-kg-ingest-contract.md) (External-extractor
  KG ingest contract) and [ADR-0010](0010-kg-edge-kind-extensibility.md) (KG
  edge-kind extensibility):** *Consistent, and explicitly a different graph.*
  ADR-0009 and ADR-0010 govern trusty-search's symbol-code knowledge graph
  (`trusty-common::symgraph`, redb `kg_nodes`/`kg_edges` rehydrated into an
  in-RAM `petgraph::DiGraph`) — a different store, different schema, and
  different consumer than the memory-core KG (`kg.db`, flat
  `(subject, predicate)` triples) this ADR discusses. A reader should not
  conflate them: nothing here proposes extending trusty-search's edge-kind
  taxonomy or its external-extractor ingest contract, and nothing in this
  ADR's KG-recall gating applies to trusty-search's graph.
- **[ADR-0031](0031-uds-for-inter-crate-transport-http-for-external.md)
  (Transport by purpose: UDS for inter-crate, HTTP for external):** *Consistent,
  and directly relevant context.* ADR-0031's INDEX entry already records that
  the observed HTTP recall divergence
  ([#5036](https://github.com/bobmatnyc/trusty-tools/issues/5036)'s
  vector-only path) is a **routing defect plus a never-enabled BM25 lane**, not
  a transport problem and not evidence for a KG lane. This ADR is consistent
  with that framing: it treats #5036 as an independent wiring fix (see
  Decision, above) and does not let the transport question influence whether
  KG content belongs in the injection block.

No other ADR in the index governs recall content composition, KG storage
keying, or knowledge-graph scope in a way this decision would conflict with.
