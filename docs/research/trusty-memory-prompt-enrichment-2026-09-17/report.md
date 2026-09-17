# Graph lookup can enrich prompts cheaply; selection still needs work

The original graph premise holds in this curated experiment: bounded entity lookup contributes facts that contextual BM25 misses, at a small warm-query cost. BM25 plus graph increased mean required-fact coverage from **73.6% to 99.1%**, with warm packet p50 moving from **2.13 ms to 2.62 ms**. Embeddings also helped BM25, reaching **96.1%** at **5.79 ms**. Adding embeddings to BM25+graph did not improve it.

This is evidence for continuing the graph approach, not production readiness. Every task-retrieval variant injected irrelevant task facts on every unsupported query. Positive-query precision was only 13.9–15.5% across the four main treatments. The next improvement should select fewer, more relevant claims and abstain when evidence does not answer the task. Adding more retrieval lanes is not the demonstrated solution.

## What was compared

[Run 01](../../../experiments/trusty-memory-prompt-enrichment/results/run-01/summary.json) completed against 1,422 initial synthetic sources, 1,434 exact-span assertions and 12 update/delete events. The 102 queries split evenly between tuning and heldout families. Each split contains 36 positive task queries, 12 task-empty queries and three standing-only queries. Each query is evaluated at 128, 256 and 512 prompt tokens. Thus each treatment has 153 packets, including 108 positive task query-budget cases and 36 negative cases; these are not 153 independent queries. Five timed heldout repetitions are timing samples, not additional quality samples.

All four main treatments use the same source text, eligibility rules and complete-claim packer. Graph edges are explicit curated assertions also present verbatim in prose. No facts are hidden from BM25 or embeddings. A local fp32 MiniLM encoder supplies embeddings; no generation or hosted inference runs. The graph experiment uses a deterministic Python projection; separate controls invoke actual native Rust graph APIs. [Protocol](protocol.md), [interface](interface.md), [fixture audit](fixture-review.md) and [verification boundaries](verification.md) preserve the design and limitations.

## Heldout comparison

Coverage and F1 are macro averages over positive task cases. All-required is the fraction of positive cases with every required group present. Tokens average all packets, including standing-only queries. Latency is warm full-packet p50/p95, with eligibility projections already prepared.

| Treatment | Required coverage | All required | Task F1 | Mean tokens | Packet p50 / p95 |
|---|---:|---:|---:|---:|---:|
| Contextual BM25 | 73.6% | 63.9% | .217 | 197.0 | 2.13 / 3.16 ms |
| BM25 + graph | **99.1%** | **97.2%** | **.249** | 207.2 | **2.62 / 3.78 ms** |
| BM25 + embeddings | 96.1% | 90.7% | .248 | 236.4 | 5.79 / 9.49 ms |
| BM25 + graph + embeddings | 98.8% | 97.2% | .235 | 241.0 | 6.19 / 10.13 ms |
| BM25 whole-source packing | 72.7% | 63.9% | .230 | 222.9 | 1.01 / 1.50 ms |
| Graph only | 65.3% | 58.3% | .263 | 104.1 | .344 / .562 ms |
| Current newest-200 lexical graph replica | 0% | 0% | 0 | 28.5 | 3.20 / 3.87 ms |
| Curated standing cache | 0% | 0% | 0 | 22.0 | .000250 / .000416 ms |

Graph-only has the highest F1 among all controls because it emits fewer facts, but misses much more required evidence. It is not a replacement for BM25. Whole-source packing is faster here because it makes fewer formatter calls, but loses coverage and uses more tokens than extractive BM25. Formatting each growing candidate packet over helper IPC is a harness cost; these measurements do not establish the cost of an optimized native packer.

All eight treatments retained every eligible acceptable standing preference (100% standing coverage). All recorded stale, forbidden and cross-scope fact counts were zero. This validates the synthetic eligibility adapter and packet checks, not the native graph's independent temporal capability.

For the four main treatments, positive-query precision was respectively **13.9%, 15.2%, 15.5%, 14.2%**. Mean useful task facts per 100 total prompt tokens on positive cases were **.715, .841, .805, .779**. Extra recall is real, but packets remain noisy.

### Budget and category effects

| Treatment | Coverage at 128 / 256 / 512 | F1 at 128 / 256 / 512 |
|---|---:|---:|
| BM25 | 68.1 / 76.4 / 76.4% | .277 / .196 / .178 |
| BM25 + graph | **97.2 / 100 / 100%** | **.347 / .218 / .183** |
| BM25 + embeddings | 92.1 / 98.1 / 98.1% | .339 / .229 / .176 |
| BM25 + both | 96.3 / 100 / 100% | .330 / .216 / .161 |

Larger budgets mostly admit more irrelevant facts after useful coverage saturates. Graph-assisted retrieval recovered exact-entity and lexical-release cases that BM25 missed in this deliberately inventory-heavy fixture. One-hop coverage improved from 50% to 100%; two-hop from 66.7% to 88.9%; paraphrase from 66.7% to 100%. The two-hop figure measures complementary BM25 and graph evidence under the selected one-hop graph policy; it does not prove standalone two-hop traversal. Graph-only nevertheless had zero required coverage for current-owner, historical-owner and hub categories under the selected seed policy. Lexical fallback matters.

Graph addition lowered category F1 on several cases already covered by BM25, including aliases and inverse relations, because it added unnecessary evidence. This is not an across-the-board quality improvement. Category samples are only three heldout queries each, repeated across budgets; do not infer general rates from them.

### The abstention failure

Every task-retrieval variant had **0/36 empty task packets** on queries labelled task-empty; the graph-only control also failed all 36. The standing cache and current lexical graph replica had 36/36, but neither supplied required positive task evidence.

Across negative cases, task-token totals were 7,246 for BM25; 7,601 for BM25+graph; 9,631 for BM25+dense; 9,664 for all three; and 2,712 for graph-only. These are valid source assertions irrelevant to the requested task, not fabricated or temporally invalid facts. Source-level ranking and entity adjacency do not by themselves answer whether a claim belongs in a particular prompt.

## What the native graph measurements establish

[Source review](research.md) found two distinct existing mechanisms. Standing prompt facts have a preformatted cache. Task-specific prompt enrichment takes the newest 200 active triples, then performs hot-predicate and lexical filtering. It does not traverse the existing resident adjacency graph. The fixture's newer inventory facts crowd useful older assertions out of that recent page; the zero task coverage of the replica is a stress-case finding, not a measured live-user failure rate.

The actual native resident graph lookup is fast. With a one-edge seed and 100, 1,000 and 10,000 unrelated noise edges, native `expand_neighbors` API p50 was **7.62, 5.21 and 6.50 microseconds**. IPC-inclusive p50 was 73.0, 49.2 and 63.8 microseconds. Native subject-prefix `query_active` API p50 was 54.8, 42.7 and 47.9 microseconds. Total graphs contained 613, 1,513 and 10,513 edges, including a separate 512-edge substantive hub.

Degree is the material risk: native one-hop expansion returned all **512 hub edges**, taking about **1.2 ms inside the API and 6.2–6.5 ms including serialization/IPC**. Hop limits do not bound fan-out. The experimental graph capped output at 32 facts and examined 32 hub edges, taking about **10–11 microseconds** locally. That is less work and less output, not a like-for-like native speedup. A hard examined-edge cap and emitted-fact cap are both needed.

Within BM25+graph, the entity-resolution/traversal stage had p50 **24 microseconds**, compared with **1.74 ms** for embedding the query in BM25+dense. Full packet cost is larger because retrieval, packing, formatting and token counting also count. [Scaling results](../../../experiments/trusty-memory-prompt-enrichment/results/run-01/scaling.json) retain all measurements and native counts.

## Tuning, maintenance and reproducibility

The frozen tuning grid selected **one seed, one entity hop, at most 128 examined edges and 32 emitted facts**. One and three seeds tied at one hop; declared grid-order tie-breaking selected one. Two-hop alternatives increased tuning recall slightly but reduced F1. Dense tuning selected cosine **0.40** over 0.25 and 0.55. The two additions were tuned separately and combined unchanged. This is the best tested policy by the predeclared objective, not a global optimum; no policy or label was revised after seeing heldout results.

Model loading took 86.7 ms; initial corpus encoding 1.45 seconds; helper startup 8.2 ms. Eighteen scope/time/scenario projections were built separately, each taking roughly 43–100 ms. Those costs are excluded from warm packet latency. No corpus-encoding truncation occurred. Python parent peak RSS was about 432 MiB; helper memory and per-treatment footprints were not measured.

Twelve events were processed in four batches of three: six revised sources were encoded and six deleted sources removed. Batch times were approximately 2.0–4.2 ms; the unchanged cycle changed and removed zero records. Incremental vectors matched a clean rebuild within absolute tolerance 1e-6. Tests additionally cover missing optional vectors, bounded backfill, source preservation and atomic retry after encoder failure. This establishes deterministic maintenance mechanics in the harness. BM25/graph clock projections still rebuild outside query timing; production incremental publication remains work to do.

[Provenance](../../../experiments/trusty-memory-prompt-enrichment/results/run-01/provenance.json) records input/source/model/helper hashes, dependency versions and build costs. Compressed packet files retain actual prompt text and verified revision/span sidecars. [Verification](verification.md) records independent approval, security review and passing tests. Direct dependency versions are pinned, but a full transitive artifact lock and helper-response timeout remain low-priority hardening gaps.

## Recommended production direction

Retain the graph as an optional, bounded prompt-enrichment source alongside BM25. Replace dependence on a newest-N global page with scoped entity/alias lookup and bounded adjacency. Keep the standing cache as its own inexpensive prelude. Keep embeddings optional: they helped this BM25 baseline, but added cost and did not improve the graph-assisted combination on this workload.

Before enabling a new prompt policy, add deterministic claim selection and abstention: use task intent and requested relation to limit eligible claim predicates, avoid indiscriminate neighbor injection, deduplicate, and stop adding facts when relevance evidence runs out. Evaluate that on a new frozen holdout, including tasks with no answer and memories extracted from real text. Do not optimize against the heldout labels already exposed here. Extraction precision and downstream answer/task success are still unmeasured.

Use dream-cycle consolidation to maintain versioned derived entity, alias, predicate and scope indices incrementally from changed source revisions. Preserve observation/validity/expiry clocks; consolidation is not evidence refresh. Publish bounded batches atomically, remove superseded/deleted derived records, and make unchanged cycles no-ops. Missing/new-format derived records must fall back to the existing source retrieval path. An optional full backfill should accelerate coverage, never be required to keep existing indexes usable.

These are recommendations and tested experimental building blocks, not installed behavior. Production integration, backward-compatible API checks, arbitrary-clock invalidation, real extraction evaluation, release measurements and installed verification remain on [#8246](https://github.com/bobmatnyc/trusty-tools/issues/8246). No live daemon, memory store or production implementation changed.
