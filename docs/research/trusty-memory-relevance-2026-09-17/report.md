# Relevance is mainly a selection and packing problem in this experiment

The fresh comparison points to query interpretation and prompt selection as the main relevance bottleneck. **Every arm retrieved 100% of required facts somewhere in its candidate set.** The failures happened when choosing which candidates belonged in the request and which fit the prompt. Storage granularity helped ordering; duplicate cleanup alone barely changed relevance.

The combined policy raised positive-query macro precision from **10.4% to 61.9%**, reduced unsupported task tokens by **93.9%**, and reduced median warm packet time from **4.43 ms to 0.70 ms**. It also lost required coverage: **77.7% to 67.9%**. Negative-query abstention improved from **0% to 60%**, short of the 90% target. **No tested policy met both 90% coverage and 90% abstention. Do not adopt the strict selector as the production default.**

## Six controlled comparisons

[Run 01](../../../experiments/trusty-memory-relevance/results/run-01/summary.json) uses 86 synthetic sources, 180 explicit assertions and eight source events. There are 48 tuning and 48 heldout queries, each split with 28 supported and 20 unsupported tasks. Every heldout query runs at 128 and 256 tokens: 96 question-budget cases per arm, including 56 positive and 40 negative cases. Timing has three repetitions after one warmup, yielding 288 measured samples per arm. Repetitions and budgets are not independent questions.

Predicates are curated, not automatically extracted. Embeddings and generation are excluded. New heldout wording was hidden from the implementer, but most task templates resemble tuning; only two heldout tasks add a second dependency branch. Results are exploratory, not a real-query population estimate. The [frozen protocol](protocol.md), [interface](interface.md), [fixture notes](fixture-notes.md), [verification](verification.md) and [interpretation guide](interpretation.md) define the boundaries.

Precision is a positive-query macro average, counting empty positive packets as zero. Duplicate useful assertions earn one credit but every emitted assertion counts in its denominator. Coverage is mean required-group coverage on positive cases; abstention is the fraction of negative cases with no task facts. Standing context consumes tokens but is excluded from these task metrics.

| Treatment | Precision | Required coverage | Negative abstention | Mean total tokens | Packet p50 / p95 |
|---|---:|---:|---:|---:|---:|
| Baseline source BM25 + generic graph | 10.4% | 77.7% | 0% | 186.5 | 4.43 / 6.18 ms |
| Claim selector only | 60.7% | 67.9% | 60% | 41.1 | .68 / .87 ms |
| Relation-aware graph only | 20.4% | **98.2%** | 0% | 184.6 | 4.21 / 6.17 ms |
| Claim BM25 index only | 12.8% | 84.8% | 0% | 186.6 | 3.29 / 4.85 ms |
| Exact duplicate cleanup only | 10.5% | 77.7% | 0% | 186.8 | 4.29 / 5.99 ms |
| Combined | **61.9%** | 67.9% | **60%** | **40.1** | .70 / .99 ms |

All treatments preserved 100% standing coverage and emitted zero recorded stale, forbidden or cross-scope facts. These are adapter/fixture results, not independent proof of native production temporal filtering.

The combined arm's task F1 rose from .176 to .636 and useful unique task facts per 100 total prompt tokens on positive cases rose from .646 to 1.843. Mean total tokens fell 78.5%. These are useful gains, but the recall regression prevents declaring an overall production win. Eight of 28 positive queries became empty in both selector arms; those eight are repeated across budgets, hence 16 empty positive cases. All-required success fell from 71.4% to 64.3%.

### Where each change helped

**Selection made the largest precision and abstention change.** Candidate coverage was 100%, but selector postselection coverage was 67.9%, equal to final coverage. Its lost facts were rejected before packing; a larger prompt budget did not recover them. Combined and selector-only had the same coverage/abstention, so adding claim indexing and graph candidate changes did not repair the strict selector's mistakes.

**Relation-aware graph improved ranking and support within the budget.** It retained 98.2% required coverage, with 100% candidate/postselection coverage. At 128 tokens it reached 96.4%, versus baseline 62.5%; at 256 it reached 100%, versus 92.9%. It still allowed the unchanged broad lexical lane to fill packets with unrelated facts, so abstention remained zero. This intervention includes new entity/path rules and different seed/hop caps; it does not isolate storage structure alone.

**Claim indexing helped packing order without solving task relevance.** Required coverage rose to 84.8%, including recovery of the instruction buried in the mixed note. Other categories regressed, including ownership with duplicates and partially supported requests. Twenty claims and twenty expanded sources are different candidate-work budgets; this is a granularity intervention, not an equal-work comparison.

**Cleanup removed repetition, not unrelated content.** It reduced redundant assertions from 67 to zero across heldout packets, but coverage and abstention stayed unchanged. Its total output actually grew by 28 tokens across 96 packets: freed space admitted more content. A packer that tries to fill the budget can replace duplicates with other irrelevant facts. Combined cleanup saved 92 tokens against the same pre-cleanup candidate/selector policy, reducing redundant assertions from eight to zero.

## Specific remaining failures

The same failure types occurred in both heldout families. These are post-run diagnosis, not changes applied to the tested policy. One implementation deviation matters: when no known intent phrase matched, the evaluated parser rejected a clause containing any negation word. The interface described a narrower three-token negation window before recognized phrases. That broader unknown-intent veto contributed to false abstentions; it is a defect of this tested implementation, not evidence that deterministic query interpretation cannot work. The frozen code and results are retained unchanged.

| Failure | Observed behavior | Needed capability |
|---|---|---|
| Instructional negation | “Do not pick a single referent,” “not the shorter annex,” or “without selecting” caused supported alias/name requests to be rejected | Distinguish negated facts/actions from instructions about how to answer |
| Entity prefix substitution | An unknown named asset or an unavailable locker fact resolved to the shorter known parent entity, yielding its owner/location | Preserve the complete requested entity; do not substitute a broader entity when the specific target is unresolved |
| Inverse relation wording | “Incoming side of the maintenance relationship” became an unknown intent and lost the required fact | Represent relation direction independently from a small list of surface phrases |
| Excluded detail treated as intent | A request for rationale that explicitly excluded the street address triggered a location answer | Bind requested and excluded information separately |
| Relation composition | Asking for release prerequisites of both dependencies returned dependency edges and the parent's release rule, missing the children’s release properties | Compile a composed request such as dependencies → release rule for each target, rather than a union of root-level predicates |
| Polarity | A request for an explicit prohibition received a positive approval requirement | Match the requested assertion polarity and qualifiers |

The remaining negative false positives were eight unique queries: two each for missing rationale, unknown entity, cutoff-excluded evidence and a missing prohibition. The selector correctly abstained on 12 of 20 negatives. Unsupported task tokens fell from 6,521 to 442 with selector-only and 396 with combined. Those residual assertions are generally valid facts answering the wrong request, not hallucinated or stale facts.

The selector's four-fact cap was not the cause of the branched prerequisite miss: the stored selected packet had only three task assertions. The path/intent policy requested the wrong endpoints. Increasing the token ceiling or fact cap therefore does not explain or repair that failure.

## Tuning and reproducibility

The predeclared four-cell grid varied unknown-intent content overlap (one or two words) and task fact cap (four or eight). Overlap one won; four and eight facts tied, so fixed grid-order selected four. On tuning, this policy had 60.7% coverage/F1 and 80% abstention. Overlap two reduced coverage to 53.6% without improving abstention. No mapping, label, threshold or cap changed after heldout results were observed.

All required facts were retrieved on this small curated corpus; that does not mean production candidate recall is always perfect. Query and packet time excludes finite eligibility/index construction and scoring. Twelve native source/claim/fallback projection builds took approximately 135–164 ms each. The helper is a debug build, not an installed release daemon. Treatment order reversed between the two budgets; no statistical confidence intervals or large-load test is claimed. Python parent peak RSS was 131.1 MiB, excluding child-process memory.

[Provenance](../../../experiments/trusty-memory-relevance/results/run-01/provenance.json) retains frozen input/policy, new and reused source, Rust source and helper hashes. Compressed per-arm artifacts retain exact prompt text, candidate/selected identities, support groups, source spans, provenance members, reasons and timings. The [result checksum file](../../../experiments/trusty-memory-relevance/results/run-01/results.sha256) covers the original result artifacts.

## What dream consolidation should do

The prototype materializes claim documents, directional postings and aliases per source revision. Initial backfill completed in eleven batches of at most eight source IDs. Eight revision/deletion events completed in three batches capped at three source IDs: four records replaced and four tombstones applied. Checkpoint replay, clean-build equality, rollback on failed publication, missing/partial/old-policy fallback and unchanged source clocks passed focused verification.

The observed no-op cycle published zero source IDs and changed no logical state, but still scanned validation data and took about 6.27 ms. Every batch copied 86 source records. Native projections are reconstructed separately. This proves prototype correctness properties, not constant-time dream cycles, bounded total resource use or incremental native production publication.

Use consolidation for these derived indices and exact duplicate groups. Keep source evidence and historical validity intact; never reset observation or verification dates during cleanup. Old indexes and partial backfills should remain readable through source fallback. A full backfill can accelerate coverage, but must not be a prerequisite to continued use.

## Next implementation priority

Retain the relation-aware candidate policy and claim granularity as promising building blocks. Replace the coarse hard intent gate with a deterministic request representation containing target entity, relation, direction, polarity, exclusions and composed paths. Use recognized structure to prioritize claims, and preserve a conservative fallback for unfamiliar supported phrasing rather than treating every uncertain parse as an empty answer. That fallback must be evaluated jointly for false positives and lost recall.

Treat these exposed failures as regression/development cases. A revised parser or packing policy needs a newly authored holdout before claiming improved generalization. Do not delete valid memories because they were irrelevant to one task. Do not promote the tested strict selector: none of the six policies passed both provisional adoption targets.

Code, fixtures and results are preserved on the experiment branch. Production retrieval, installed daemons and live memory remain unchanged. Integration, backward compatibility, release measurements and installed verification remain on [#8246](https://github.com/bobmatnyc/trusty-tools/issues/8246).
