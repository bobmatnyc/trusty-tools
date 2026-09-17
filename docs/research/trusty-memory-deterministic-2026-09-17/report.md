# Deterministic memory retrieval: experiment results

Revision-aware repair and explicit temporal eligibility are the strongest results. Structural context recovers three missing sources. Chunking reduces payload but worsens ranking in one category; KG adds no recall or MRR gain on this fixture. The isolated experiment is complete, with production integration still outstanding.

The experiment implements an isolated retrieval and maintenance engine using Trusty's real BM25 implementation. All memories are synthetic. No embeddings, inference, live memory database, installed daemon, or normal dream cycle participates. The production completion work is tracked in [#8246](https://github.com/bobmatnyc/trusty-tools/issues/8246).

## What was tested

The corpus contains 102 source memories across six entity families, with 18 revision events covering body changes, metadata changes, and deletion. There are 33 tuning queries and 33 held-out-family queries; each split contains 23 answerable queries and 10 queries without a labelled relevant source. Both splits use shared templates. This is a test of mechanisms on authored fixtures, not an estimate of general retrieval quality.

Seven predeclared configurations vary context budget, chunk size, freshness weight, and KG weight. Selection prioritizes scope and temporal validity, excerpt integrity, source hits/recall/MRR, then card tokens and snapshot bytes. Ties retain the first configuration. Six cumulative treatments compare raw BM25, revision repair, structural context, chunking, temporal rules, and scoped KG expansion. Labels never enter engine requests.

Run-01 exposed a chunk-length defect: the shared BM25 tokenizer returns unique vocabulary, so counting its whole-body output did not bound repeated words. Run-02 uses occurrence counts and a 4096-byte source-child limit. The [amendment](amendment-01.md) records this correction. Run-01 had already exposed the held-out results; run-02 is a re-evaluation after that correctness fix, not a newly unseen holdout. Fixtures, labels, split, and parameter grid were unchanged.

## Held-out-family re-evaluation

Run-02 completed successfully: all seven tuning configurations and six ablations passed the runner's source-preservation, exact-locator, deterministic-response, unchanged-maintenance, and read-only-state checks. MRR and recall below cover the 23 answerable queries; invalid-query counts cover all 33. Each answerable query has one labelled relevant source, so source hit rate equals mean Recall@5 here.

| Treatment | Hits@5 | Recall@5 | MRR | Queries with invalid facts | Stale-index hits |
|---|---:|---:|---:|---:|---:|
| Raw BM25 | 20/23 | 0.8696 | 0.6522 | 3 | 3 |
| Revision repair | 20/23 | 0.8696 | 0.6522 | 3 | 0 |
| + Structural context | 23/23 | 1.0000 | 0.7826 | 3 | 0 |
| + Chunking | 23/23 | 1.0000 | 0.7174 | 3 | 0 |
| + Temporal rules/ranking | 23/23 | 1.0000 | 0.7826 | 0 | 0 |
| + Scoped KG | 23/23 | 1.0000 | 0.7826 | 0 | 0 |

All treatments have zero observed scope violations, invalid revision/byte/line locators, and grouped duplicate hits. Historical-invalid queries are 0/6 throughout. Current-invalid queries fall from 3/24 (12.5%) to 0/24 with temporal treatment. These are bounded labels, not proof that every returned assertion is true or current.

Raw/repaired misses are `velvet-kg-hop`, `meadow-kg-hop`, and `tundra-kg-hop`. Context alone recovers them, so this fixture cannot demonstrate an additional KG recall advantage: lexical context overlaps with the graph questions. The three invalid-current cases are the corresponding `*-owner-current` queries. Chunking moves the relevant protected-task source from rank 1 to rank 2 in all three families, reducing that category's MRR from 1.0 to 0.5; it does not improve the manual category's MRR of 0.3333. Temporal filtering improves owner ordering and removes the old occupants, offsetting the aggregate ranking loss without fixing task ordering.

All treatments return unrelated candidates on **10/10 empty-label queries**: `*-cutoff`, `*-expiry`, `*-unsupported` for the three held-out families, plus `tundra-after-deletion`. The expired, future-known, and deleted sources themselves remain excluded. Correct empty-result rate is 0%; nonempty-result rate is 100%. Candidate retrieval is not answer generation, but a consumer cannot treat every nonempty result as an answer.

| Treatment | Card tokens | Full-body projection tokens | Actual hit-JSON tokens | Snapshot bytes | CLI p50 / p95 ms | Child CPU seconds |
|---|---:|---:|---:|---:|---:|---:|
| Raw | 21,664 | 10,680 | 34,026 | 45,423 | 117.0 / 122.5 | 13.55 |
| Repair | 21,664 | 10,680 | 34,020 | 45,423 | 117.3 / 122.0 | 13.39 |
| Context | 22,134 | 10,786 | 34,740 | 49,279 | 121.4 / 127.0 | 13.95 |
| Chunks | 19,346 | 19,886 | 31,959 | 52,489 | 564.1 / 579.2 | 65.12 |
| Temporal | 19,231 | 19,126 | 31,800 | 52,489 | 563.9 / 577.4 | 65.09 |
| KG | 18,631 | 15,490 | 31,208 | 52,489 | 557.1 / 577.8 | 64.65 |

Token totals span all 33 queries. Chunking reduces cards by 12.6% relative to context, but changes which sources are returned, including long distractors; the full-body totals therefore change too. With KG, cards are still 20.3% larger than the simpler full-body projection for the same returned sources. Rich provenance does not automatically save tokens on short memories. The process-wide peak child RSS is 12,419,072 bytes on this macOS run, not a per-variant measurement. CLI timings are explained below and do not support a production speedup claim.

Maintenance ends with all 96 remaining source records preserved after six explicit deletions. Each repaired treatment publishes 102 initial sources plus 12 changed sources and removes six deleted sources, with zero pending repairs. Raw publishes only the initial 102, removes the same six, and retains 12 pending stale revisions/metadata fingerprints. Chunked snapshots hold 126 child rows for those 96 sources; unchunked snapshots hold 96. Work counters count source publications, not child rows. An unchanged pass makes zero index mutations. Resume, tombstone ordering, mixed legacy state, and restart checks are separately covered by the example tests.

## Tuning result

The selected tested policy is context budget **64**, chunk bound **128 lexical occurrences**, freshness weight **0.05**, and KG weight **0.15**. Every cell retrieved all 23 labelled tuning sources, with zero labelled temporal/scope/excerpt errors. Selection therefore turned on ranking and payload. Cells 2 and 3 tied exactly on the selection objective; the predeclared first-cell tie break chose 2. The data does not establish that 64 context terms are better than 32.

| Cell | Context | Chunk bound | Freshness | KG | MRR | Card tokens | Snapshot bytes |
|---|---:|---:|---:|---:|---:|---:|---:|
| 0 | 64 | 256 | 0.05 | 0.15 | 0.7826 | 20,248 | 50,563 |
| 1 | 32 | 256 | 0.05 | 0.15 | 0.7826 | 20,248 | 50,563 |
| **2** | **64** | **128** | **0.05** | **0.15** | **0.7826** | **18,697** | **52,489** |
| 3 | 32 | 128 | 0.05 | 0.15 | 0.7826 | 18,697 | 52,489 |
| 4 | 64 | 256 | 0.00 | 0.15 | 0.7826 | 20,248 | 50,563 |
| 5 | 64 | 256 | 0.15 | 0.15 | 0.7826 | 20,388 | 50,563 |
| 6 | 64 | 256 | 0.05 | 0.30 | 0.7717 | 20,248 | 50,563 |

The smaller chunks reduce tuning card tokens by 7.7% versus the default cell, while increasing snapshot bytes by 3.8%. Stronger KG weighting worsens ranking; zero freshness weight ties the default on the selection objective. Thus temporal eligibility has stronger support than any particular recency boost. This limited grid is not a global optimum search, and it does not test every interaction between parameters.

The cumulative ablations do not isolate temporal eligibility from freshness weighting, or compare unsplit contextual memories plus temporal rules against the chunked temporal variant. Those are the next controlled comparisons. Retain chunking as optional until that tradeoff is resolved; keep KG experimental until graph-only questions show a gain.

## Implementation and compatibility

Maintenance fingerprints source revisions, relevant metadata, dependencies, and enrichment policy. It upserts complete child generations, removes obsolete children, respects tombstones, saves a stable resume cursor, and leaves unchanged generations alone. An independent Python oracle verifies every maintenance result against the original source/event stream. Excerpts carry exact revision-bound byte and line locations; source text is never rewritten by this pass.

The existing public backfill regression demonstrates the repair need: a changed body under an already-indexed ID returns `AlreadyIndexed`, leaving the old term searchable and the new term absent. This is reproduced against the checked-out implementation, not diagnosed against the installed daemon.

The example accepts missing derived metadata as legacy coverage and tests mixed legacy/derived rows, restart, and the existing `{doc_id,text}` snapshot reader. Production source schemas, APIs, and behavior are unchanged because the implementation lives under `examples/`. These checks do not prove old-release or old-client compatibility for a production integration. In particular, child-to-source resolution and opt-in response fields still require integration tests.

The intended rollout needs no mandatory full reindex: retain the existing source-level BM25 path, enrich new/changed records, and upgrade old records in resumable dream batches. An optional full backfill only accelerates coverage. Keep derived child indexes/version metadata separate from the old reader's authoritative source identifiers until an adapter explicitly resolves them. Missing, unsupported, or incomplete enrichment must fall back to a complete legacy generation.

## Performance boundary

The portable engine receives and returns complete JSON state. The recorded executable is a debug build. Each timed request starts a fresh process, validates that state, regenerates claimed-current children for integrity checking, reconstructs BM25, runs one query, and serializes a response. The chunker also repeatedly counts growing candidate spans. These are development-harness costs; the timings are not installed-daemon query latency or release-build throughput.

Publication budgets bound selected documents, emitted bytes, and inspected dependencies. They do not bound total parsing, validation, reconstruction, or serialization work. A production adapter must persist derived state, perform checked publication on the write/maintenance path, and query the resident index. Linear occurrence accounting and cached dependency fingerprints are required before making efficiency claims at production scale.

Navigation-card token totals include exact returned excerpts and provenance. Full-body totals use the same returned source IDs with their complete current bodies and a smaller metadata projection. This comparison measures payload tradeoffs; it does not establish equivalent answer quality, follow-up-read savings, or end-to-end agent efficiency. No answer generation was evaluated.

Relevance is labelled at source level. Exact byte/line validation proves an excerpt came from the cited revision; it does not by itself prove that the excerpt answers the question. Required/forbidden excerpt terms are specifically labelled for same-ID updates, not comprehensively for every question. Broader excerpt-answer coverage needs separate labels before another evaluation.

Pre-grouping duplicate share and expansion follow-up reads were not instrumented. Final grouped duplicate counts are measured. Child CPU covers the whole variant; peak RSS is the maximum child process across the complete run, not per-treatment memory growth. These are explicit gaps against the broader research measurement list.

## Production completion order

1. Integrate revision-aware upsert/delete after durable source writes and deterministic dream maintenance. Dreaming repairs missed notifications; it must not be the only route to freshness. Persist a dirty queue, dependency fingerprints, and bounded reconciliation cursor. Recheck source revisions before atomic publication.
2. Preserve legacy reads and API defaults. Put optional derived children/cards behind an explicit capability, retain original source IDs, and validate rollback with old-release snapshots and old clients. Test interrupted publication and concurrent edit/delete races in the real daemon.
3. Add explicit current/as-of/general modes using existing temporal metadata. Recorded time, effective time, verification time, access time, and consolidation time remain distinct. Only declared single-valued fact slots establish supersession; arbitrary prose cannot be resolved deterministically.
4. Evaluate chunk size, context, and KG independently on a broader time-stratified corpus. Include alias-only and relation-only queries that lexical context cannot already solve, long memories with multiple relevant sections, conflicting assertions, retained history, and unsupported queries. Freeze a new unseen holdout before further tuning.
5. Measure release-build resident-daemon retrieval latency, maintenance CPU/RSS, actual read/write amplification, and response consumption. Keep promotion contingent on measured gains and compatibility, rather than treating the synthetic winner as universally optimal.

## Reproduction and retained evidence

The [run-02 summary](../../../experiments/trusty-memory-deterministic/results/run-02/summary.json) contains all exact metrics and runtime/source hashes. The [selection](../../../experiments/trusty-memory-deterministic/results/run-02/selection.json) was written after tuning and before the six ablations. Each compressed artifact retains per-query results, category/scenario metrics, maintenance observations, final state, and semantic hashes. [Run-01](../../../experiments/trusty-memory-deterministic/results/run-01/summary.json) remains visible as superseded evidence; its historical source hashes are recorded, but only the corrected v2 source is delivered here.

Final binary SHA256: `679b2842c2891afa9707a58fb68f4fd01ec7f9bdf91c267b68d4f42f09a0cd19`. All 13 recorded implementation/interface hashes were rechecked against the final worktree after the run. The recorded Git revision is the experiment's pre-commit parent; those file hashes identify the actual tested, then-uncommitted implementation. The enclosing commit preserves that source. The original frozen input manifest still verifies.

See the [interface](interface.md), [frozen protocol](evaluation.md), [verification evidence](verification.md), and [reproduction instructions](../../../experiments/trusty-memory-deterministic/README.md). No merge, installation, or production completion is claimed.
