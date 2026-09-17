# Relevance run-01: independent numbers audit

Read-only artifact audit, 2026-09-17. I authored the fixture; this is an independent recomputation of results, **not independent dataset validation**. No retrieval/ranking rerun, policy change, source edit, or Git action.

## Verified results

Independently reconstructed semantic duplicate keys from authoritative source/fact records, required-group coverage from gold, unique acceptable semantic precision against all emitted task assertions, duplicate counts, candidate coverage, and latency medians. Checked every emitted claim, source span, revision and inclusion in packet text across all six arms ×96 cases. Recomputed values match stored metrics; medians are serialized as integer nanoseconds, dropping a possible .5ns.

| Arm | Positive precision | Positive coverage | Negative abstention | Packet p50 ms |
|---|---:|---:|---:|---:|
| Baseline |.1044240132|.7767857143|0|4.433229|
| Selector |.6071428571|.6785714286|.6|.683499|
| Relation graph |.2038065605|.9821428571|0|4.207917|
| Claim index |.1279664109|.8482142857|0|3.286562|
| Cleanup |.1054152911|.7767857143|0|4.291812|
| Combined |.6190476190|.6785714286|.6|.697208|

Denominator per arm:48 unique heldout queries;28 positive and20 negative; two budgets yield56 positive and40 negative cases. Three timing repetitions yield288 timing samples, not independent query samples. Coverage is macro per positive query-budget case, not pooled required groups. All positive candidate-coverage values are1.0, including the cases that ultimately fail. Candidate absence is not the cause of the observed recall losses.

Combined required coverage **regresses by9.8214percentage points** versus baseline. Positive all-required success falls from71.4286% to64.2857%. Its F1 increases from.175672 to.636054 because precision improves considerably, but that cannot hide recall loss. None of the six arms reaches both90% positive coverage and90% negative abstention. Relation-aware graph reaches98.2143% coverage yet never abstains on negatives.

Stored unsupported task tokens fall from6521 baseline to396 combined over40 negative cases. Mean total prompt tokens fall186.46875→40.125. All arms retain standing coverage1.0 and record zero stale/forbidden/scope violations. Those safety metrics do not make irrelevant current facts useful.

## Where selection fails

Both families show the same repeated patterns; counts below are unique queries, not duplicated budget cases.

**False negatives / incomplete positive answers:**

- Two inverse-maintenance queries: intent is `unknown`; supported candidate discarded below threshold.
- Two ambiguity-list queries: “Do not pick a single referent” is marked `negated_demand`, and the follow-on sentence lacks resolved entity context. No alias evidence survives.
- Two exact longer-prefix location queries: the clarification “not the shorter similarly named annex” suppresses the whole affirmative location request as `negated_demand`.
- Two alias-resolution queries: “without following other relationships” is treated as negation of the requested alias expansion.
- Two branched prerequisite comparisons retain both dependency edges but omit the properties of the reached dependencies; coverage.5. A release requirement of the parent entity appears instead. This is incomplete relation/entity binding, despite all required evidence existing in candidate lists.

Thus8 unique positive queries become fully empty and2 become partial. The same results at both budgets indicate selection loss rather than insufficient token budget. Postselection coverage equals final combined coverage.678571.

**False positives on8 of20 unique negative queries:**

- Missing rationale for the annex emits its location because “its address would not answer this” activates location intent.
- Unknown Observatory entity resolves to the shorter known base entity and emits that entity's owner/maintainer.
- Future-known Locker location is excluded, but the query resolves to the base entity and emits its location. No temporally invalid fact is emitted; the wrong entity answers the task instead.
- Explicit prohibition request emits an ordinary release approval prerequisite, conflating positive prerequisite with forbidden action.

The remaining12 negative queries abstain. Keyword recognition, clause context, negation attachment, and exact entity binding are unresolved. These observations support a next independently frozen test; they must not motivate tuning against this heldout set.

## What the ablations establish

- Relation-aware graph materially improves ranking/packing of available evidence without a selector:98.2% coverage versus77.7%, with no abstention gain. This is the strongest recall result, not a finished relevance policy.
- Claim indexing improves coverage to84.8% and reduces packet latency, but still fills unsupported packets. Smaller indexing units are insufficient for abstention.
- Selector supplies almost all abstention and precision gains; combined has the same recall and abstention as selector-only. Added mechanisms slightly improve precision/duplicate handling, not the failed intent/entity cases.
- Cleanup removes67 baseline redundant emitted assertions but does not improve coverage or abstention. Its mean tokens slightly increase because other candidates backfill freed space. `duplicate_token_difference=-28` for cleanup means28 more total tokens than its paired uncleaned packet, not negative duplicate removal. Combined's paired difference92 is a net packet-token difference; do not call it exact removed duplicate-token volume without qualification.
- Lower latency includes fewer formatting/packing operations on shorter packets. It is an end-to-end experimental packet result, not a native scorer speedup.

Fixture limitations remain: curated facts, mostly shared scenario primitives between tuning/heldout, and structural novelty confined to2/48 heldout dependency-comparison queries. No embeddings, automatic extraction, generated answers, or production service behavior are evaluated. Prior benchmark artifacts remain separate.

Parent continues report and delivery. Audit artifact: `/tmp/relevance-results-audit.md`.
