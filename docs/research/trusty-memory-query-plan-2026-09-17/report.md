# Memory query-plan experiment results

The structured policy met the predeclared 90% coverage/90% abstention target on this fresh synthetic holdout. It completed 18 of 20 positive queries and correctly abstained on all 12 negative queries, at both 128- and 256-token budgets. The prior policy completed 8 of 20 positives and abstained on 4 of 12 negatives. No embeddings were used.

## Heldout comparison

| Treatment | Positive coverage | Positive precision | All required evidence | Negative abstention | Mean packet tokens | p50 / p95 ms |
|---|---:|---:|---:|---:|---:|---:|
| Previous combined policy |60%|47.5%|40%|33.3%|49.53|0.600 /0.942|
| Defect fixes, flat requests |65%|71.7%|50%|100%|37.78|0.368 /0.655|
| Structured directed requests |90%|90%|90%|100%|38.88|0.370 /0.595|

Coverage and precision are macro averages over positive queries; an empty positive result scores zero. The 32 unique heldout queries contain 20 positives and 12 negatives. Each runs at two budgets, giving 64 query-budget cases per arm and 192 measured timing samples after warmup. Quality and packet lengths were identical at the two budgets; neither had packet-support loss. Per-budget latency and all raw cases are saved with the [results](../../../experiments/trusty-memory-query-plan/results/run-01/summary.json).

The repaired and structured arms had exactly matching candidate IDs/order for every query-budget case. Their required-evidence candidate coverage was 100%, compared with 90% for the old control. Structured binding added 25 percentage points of final coverage over the flat repaired selector. Control-to-repair bundles candidate and selection changes, so it is not a pure repair-only ablation. This experiment does not independently compare BM25 against graph retrieval.

On the 16 structurally novel heldout queries, coverage was 62.5% for the old policy, 68.75% for flat repairs and 100% for structured requests. These are eight structures instantiated twice, not 16 independent structures. The [independent result audit](evidence/results-independent.md) recomputed semantic metrics and checked emitted source state.

## What improved and what still fails

The structured policy correctly binds different properties to different named entities, follows inverse maintenance before requesting a service property, excludes the unwanted dependency branch, expands ambiguous aliases with complete support, and follows dependency-to-release paths. Exact duplicate owners no longer consume slots needed by other requested properties. Full entity references and literal prohibition qualifiers prevent inappropriate parent-entity or approval-rule answers.

Only the two heldout unfamiliar-wording ownership queries remain misses: the phrase “ultimate custodial accountability” is outside the frozen grammar. Their ownership evidence was present in candidates; parsing abstained. No policy or label was changed after seeing these results. The next relevance work is broader query interpretation or explicit caller-supplied relation structure, followed by a new real-query holdout—not another index expansion justified by these misses.

The partial-branch cases correctly report partial execution: one dependency lacks a stored release property. All answerable gold groups are nevertheless present. A partial execution status and complete answerable-evidence recall describe different things.

All arms had zero stale or cross-scope emissions and complete standing-context retention. The previous arm returned a query-forbidden wrong-qualifier prohibition in two unique queries, counted four times across budgets. Both new arms returned zero forbidden facts. These forbidden labels are query-compatibility checks, not evidence of temporal or authorization leakage. Negative-query task tokens fell from 498 to 0 across the 64 heldout cases. The structured mean packet was 21.5% smaller than the old control. Every arm emitted zero exact duplicates on this holdout; the duplicate repair's value is recovering capacity before selection, not merely reducing final duplicate counts.

## Scope and operational evidence

There are 64 authored queries overall, split 32/32. Sixteen heldout queries use eight compositions absent from tuning, instantiated in two families. The other 16 reuse scenario primitives. The fixture author saw the frozen grammar, and this is not a representative natural-language workload. Tuning-side coverage was 88.9% for both new arms because two positive paraphrases were unsupported; no parameter tuning was performed. Crossing the heldout target is a bounded experiment result, not production-readiness evidence.

Ninety-two initial sources contain 100 assertions; eight events replace four release sources and tombstone four archive sources. Independent audits validated raw source states, exact spans, gold groups and split separation. Reused deterministic maintenance passed checkpoint/replay and incremental-versus-clean equivalence checks. Ten finite scope/time projections took 129–143ms each outside query timing. The no-op maintenance step changed nothing but still copied 92 source records and validated 656 derived items, taking 4.36ms. These costs remain relevant to production dream-cycle design.

Timing uses the existing native BM25 debug helper with Python-directed graph traversal. Python peak RSS was 109.1MiB, excluding the helper. This is not installed-daemon latency or resource proof. Production APIs, stored schemas and daemons are unchanged; no full reindex is required to retain current behavior. Production integration and incremental backfill remain separate work under [#8246](https://github.com/bobmatnyc/trusty-tools/issues/8246).

## Reproducibility and verification

The final 38-input manifest freezes new and reused code, five fixture files, specifications, tests and helper source/binary before official ranking. Prior experiments remain immutable. Nineteen focused tests and strict type checking passed. Independent review approved the corrected exclusion, scope/clock and candidate-status findings. Security guards passed with model constructors, Python network access and official fixture reads blocked during tests; the reserved-marker parser crash was repaired before measurement. See [verification](verification.md), [interpretation limits](interpretation.md), [protocol](protocol.md), and [fixture notes](fixture-notes.md).
