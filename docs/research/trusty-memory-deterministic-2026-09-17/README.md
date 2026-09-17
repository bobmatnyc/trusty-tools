# Deterministic memory retrieval and dream-cycle indexing

Status: source-backed research and experiment proposal, not implemented or benchmarked. Inspected source revision: `90b6aeb944e1d010f3690281ec24efe7b89435c3`. Scope: chunking, BM25, temporal metadata, and KG; no embeddings or inference in the experimental path. This follows the [search experiment](../trusty-search-context-2026-09-17/README.md).

## Recommendation

Use dreaming as incremental maintenance of derived retrieval data. First reconcile lexical index content, then add bounded structural context, then evaluate temporal ranking and KG expansion independently. Keep original memories and their provenance authoritative. Consolidation time is not evidence that a fact is current.

The first version should produce extractive memory cards and indexes, not synthesized facts. Deterministic processing cannot reliably decide whether arbitrary prose contradicts or supersedes another statement; only explicit structured keys, revisions, and supported relationship rules should establish that.

## What exists

Paths refer to the inspected source revision. These are source observations, not findings about the installed daemon.

| Area | Evidence | Implication |
|---|---|---|
| Memory metadata | `crates/trusty-common/src/memory_core/palace.rs:219` defines Drawer with creation/access timestamps, type, expiry, fact key, source, tags, and derived content hash | Much of the indexing input already exists; access time is not verification time |
| Temporal ranking | `memory_core/decay.rs:36` applies age-based decay and bounded access boost; `retrieval/layers.rs` uses it | Extend/test existing temporal policy rather than adding a second blind recency multiplier |
| Dream processing | `memory_core/dream/dreamer.rs:204` orchestrates prune, embedding-backed dedup, vector compaction, closet refresh, optional inference consolidation, flush, and KG compaction | Existing dream cycle is not a no-embedding deterministic baseline; add an independently selectable maintenance pass |
| Keyword navigation | `memory_core/dream/helpers.rs:36` extracts deterministic keywords; `build_closet_index` builds navigation postings | Reuse structural vocabulary, but evaluate identifier tokenization; stripping punctuation can lose useful boundaries |
| Content changes | `memory_core/dream/helpers.rs:125` merges content/tags into a surviving drawer and updates its content hash | A stable drawer ID does not prove its BM25 document is current |
| Lexical backfill | `crates/trusty-memory/src/bm25_backfill.rs:276` emits body text; `:369` skips work when all IDs exist | Coverage by ID cannot establish content or enrichment-version freshness |
| BM25 persistence | `crates/trusty-memory/src/bm25_index.rs` stores stable `{doc_id,text}` snapshots and supports upsert/delete | Derived enrichment can preserve the existing snapshot representation |
| Candidate fusion | `tools/bm25.rs:275` boosts existing recall hits; normal `tools/recall_ops.rs:236` uses it | A BM25-only experiment needs a real lexical candidate path, not empty vector results fed into boost-only fusion |
| Lexical hydration | `tools/bm25.rs:331` and warm-up path in `tools/recall_ops.rs` hydrate lexical candidates | Reuse the mechanism, with consistent scope/expiry filtering; warm-up fallback is not itself the controlled benchmark interface |
| Temporal KG | `memory_core/store/kg_store.rs` defines supersession policy/history and validity intervals; `store/kg/ops.rs` has active-triple queries | Reuse existing temporal semantics and predicate cardinality; do not impose latest-wins on multivalued relations |

Unless otherwise prefixed, `memory_core/` above is under `crates/trusty-common/src/`. The apparent dream-to-BM25 refresh gap is an integration hypothesis requiring a regression test, not a reproduced production incident. ID-only backfill's inability to detect changed text is directly visible in its implementation.

## Deterministic derived representation

For each drawer, derive a bounded search document from original text plus explicit room/palace context, tags, fact key, and structured KG aliases within the same permitted scope. Deduplicate terms and cap every added field. Never propagate an entire room's vocabulary into every drawer.

Keep small atomic memories intact. Split long memories only at existing headings, paragraphs, or list boundaries, with a fixed maximum token count and deterministic fallback. Each derived child retains drawer ID, content digest, and byte/line range. Do not combine unrelated dated observations into one ranking unit. Rank children, then group by original drawer so long memories do not monopolize top-k. Compare unsplit enriched documents first; chunking is a separate treatment.

A card contains an exact excerpt, source locator, recorded date, explicit effective/verification dates when available, expiry/current-history status, and revision digest. Line locators are relative to a specific drawer revision, not stable across edits. No abstractive summaries, invented aliases, inferred dates, or guessed fact keys.

## Dream maintenance algorithm

1. Select a stable ordered batch of dirty drawer IDs, plus a bounded rolling scan for old rows with no enrichment metadata. Persist the cursor so later IDs cannot starve. Bound documents, bytes, and KG edges; a wall-clock timeout is a safety stop, not a semantic selection rule.
2. Snapshot each source revision and its dependency digests. Fingerprint body, relevant tags/room metadata, explicit temporal fields, relevant KG aliases, tokenizer version, and enrichment policy version. Existing content_hash alone is insufficient because metadata can change without the body changing.
3. Construct the document/card/postings with pure functions. Same source snapshot, policy, and explicit as-of time must yield identical outputs and ordering. Use stable ID tie-breaks; never depend on hash-map iteration.
4. Before publishing, verify the source revision still matches. Retry concurrent edits. Commit related derived outputs as one generation, or record pending repair and keep the previous complete generation available. A deleted ID/tombstone must win over queued updates so maintenance cannot resurrect forgotten content.
5. Upsert changed documents, remove dead derived references, and mark the generation complete only after durable success. Check coverage by revision and policy version, not count or ID alone. Coalesce disk flushes; do not rewrite the full BM25 snapshot per drawer.
6. Resume after interruption without duplicate documents, lost cursor work, or false completeness. A second pass on unchanged inputs makes no index mutations.

Integration should follow successful source persistence, through a daemon-side adapter/change batch because the core Dreamer does not own the daemon BM25 lane. Run on normal writes as well as dreaming; dreaming repairs missed work and gradually upgrades older entries. Room renames and KG alias changes enqueue dependent drawers. Periodic bounded reconciliation catches missed notifications. This avoids making dream frequency a correctness requirement.

Do not run existing embedding-backed dedup/vector compaction/semantic consolidation when measuring this pass. Existing production modes remain available and unchanged. No model initialization, embedding, or model scoring is allowed in the experimental mode.

## Time-sensitive retrieval

Separate recorded time, asserted effective time, explicit verification time, last access, and index-build time. Missing dates remain unknown. KG validity intervals represent recorded assertion history unless the writer explicitly supplied event-time semantics; they are not automatically real-world effective dates.

Use three explicit query modes in the experiment: current state, as-of history, and general relevance. Select them by a structured option first; narrow deterministic syntax recognition may be evaluated separately. Resolve relative dates against an injected clock and timezone.

- Current state: apply explicit expiry/supersession rules before ranking. Prefer the active occupant of a known fact slot. A newer unrelated observation must not suppress an older relevant fact.
- As-of: traverse retained validity intervals and retrieve provenance for the requested date. Exclude observations not yet available at the evaluation cutoff when simulating what the system knew then. Report gaps where retention/deletion removed history; an index cannot recreate it.
- General relevance: use BM25 and bounded temporal KG candidates with a modest, type-aware freshness component after relevance. Preserve durable preferences, identity, decisions, and protected task semantics. Existing TTL deletion rules remain distinct from ranking decay.

A candidate scoring experiment can blend normalized/rank-based lexical evidence, typed KG relevance, existing importance, and a bounded freshness term `2^(-age/half_life[type])`. Freeze weights/half-lives on tuning data and inject `as_of`; do not multiply raw BM25 scores by arbitrary recency weights. Use explicit last-confirmed/effective dates when available and label fallback to recorded time. Consolidation and recall never advance confirmation time. Unknown types use a conservative default, not guessed volatility from free text.

Exact duplicate grouping may reduce repeated results without deleting source records. Near-duplicate grouping must not merge different dates, negations, quantities, scopes, or fact-slot values. Only explicit, policy-supported single-valued relationships may establish supersession; ambiguous conflicts remain visible.

## Head-to-head experiment

Use isolated synthetic fixtures first, with known facts and timestamps, plus a separately reviewed de-identified memory snapshot if needed. Do not commit private memory contents. Freeze queries, relevance/validity labels, corpus hash, policy version, and as-of times before tuning. Split by entity/fact family and chronological cutoff, not merely random paraphrases.

| Treatment | Change from baseline |
|---|---|
| A | Existing raw-body BM25 over the same live fixture IDs; no vector fallback |
| B | A plus revision-aware dream reconciliation after edits/deletes/consolidation |
| C | B plus bounded structural context; same chunk boundaries |
| D | C plus structural splitting and grouped extractive cards |
| E | Best lexical treatment plus typed temporal ranking |
| F | E plus bounded, scoped temporal KG candidate expansion |

Evaluate initial load, unchanged repeat cycle, same-ID content update, metadata-only update, supersession, expiry, deletion, partial-budget resume, restart, and out-of-order updates. Pin the clock at multiple dates. Include stable old preferences, old relevant decisions, conflicting state changes, historical questions, ambiguous aliases, and mixed enriched/legacy rows.

Report Recall@5/MRR with source labels, stale-current-fact rate, historical-validity errors, duplicate share, payload tokens, expansion follow-ups where measured, p50/p95 latency, cycle CPU/RSS, documents rewritten, index size, and coverage by revision/version. Freshness mistakes matter separately from relevance. Require zero embedding/inference calls, zero expired/superseded answers presented as current on labelled fixtures, no source loss, no scope leak, deterministic reruns, and restart compatibility. Do not call synthetic wins general retrieval gains.

## Backward compatibility and delivery order

1. Add revision-aware lexical reconciliation and the isolated deterministic mode; demonstrate changed text under an existing ID gets repaired.
2. Add optional derived context/cards with old response defaults preserved and no authoritative memory schema migration. Store optional versioned derived metadata separately; missing metadata means legacy coverage, not an invalid database.
3. Add temporal ranking/KG treatments and measure them independently. Keep existing source IDs and TTL/task protections intact.
4. Promote only measured improvements; document tradeoffs and add real old-release fixture/old-client upgrade tests before production enablement.

No full reindex is required to start or serve existing memories. New writes get enrichment immediately; dream batches upgrade unchanged memories gradually. An optional backfill accelerates full coverage. Type-aware time scoring can operate immediately on existing metadata, while better context coverage accrues over cycles. Rolling back the optional layer must leave the authoritative memory and old BM25 snapshot readable.

## Research context and limits

[Generative Agents (Park et al., 2023)](https://arxiv.org/abs/2304.03442) combines memory retrieval with reflection and temporal behavior, but its architecture uses language models; it is not evidence that deterministic consolidation alone achieves the same outcome. [Anthropic Contextual Retrieval](https://www.anthropic.com/engineering/contextual-retrieval) adds context to BM25 and embedding inputs; its reported combined gains cannot be attributed to this proposed no-embedding treatment.

The deterministic contribution here is derived-index maintenance, structural context, explicit temporal eligibility, and reproducible ranking. Semantic truth verification and open-ended conflict resolution remain outside what these rules can establish. No memory benchmark or live dream cycle was run for this assessment.
