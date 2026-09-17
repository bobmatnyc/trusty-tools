# Query-plan run-01 independent results audit

Read-only audit, 2026-09-17. I authored the fixture and independently recomputed results from emitted packets and canonical gold. This is a numbers/provenance audit, not a second independent dataset review. No ranking, policy change, or source edit.

## Verdict: numerical and evidence checks pass

| Heldout arm | Positive precision | Required coverage | Negative abstention | p50 ms |
|---|---:|---:|---:|---:|
| Old combined |47.5%|60%|33.3333%|.600250|
| Defect fixes |71.6667%|65%|100%|.368146|
| Structured plan |90%|90%|100%|.369833|

Recomputed semantic duplicate classes, useful-assertion precision and required-group recall from raw sources/gold, not the implementation's evaluator. All values match packet metrics. All evidence claims match source byte spans and appear in actual packet text. Independent event replay and parsed UTC comparisons verify scope, observation cutoff, TTL, valid intervals, current revision and tombstones for every emitted assertion.

Each arm has32 unique heldout queries:20 positive,12 negative. Two budgets produce40 positive and24 negative cases. Three repetitions produce192 timing samples. Both budgets have identical quality; packet size is not the remaining constraint. Structured precision/coverage/F1 equal90% because18 unique positive questions are fully correct and2 produce no task evidence. Every negative produces an empty task portion. Standing evidence remains present, so “empty” does not mean an empty whole prompt.

The structured arm reaches the provisional90% coverage/90% abstention target on this small authored heldout. This is not production readiness or an estimate of unrestricted-language accuracy. There are only12 unique negative questions, each family repeats templates, and positive tasks mostly follow the published bounded grammar.

## Matched candidates and structural subset

The repaired and structured arms have exactly identical ordered candidate IDs in all64 heldout query-budget cases. Their candidate required coverage is100%; old combined has90%. Thus the **65→90% repaired-to-structured gain** is evidence for ordered binding and selection with shared candidate generation. The old-to-new comparison also changes candidate retrieval and cannot isolate parsing alone.

For the16 structurally novel heldout questions (32 budget cases), required coverage is:

- Old combined:62.5%.
- Defect fixes:68.75%.
- Structured plan:100%.

These are eight new compositions instantiated in two families, not16 independently sampled structural designs. The structured executor preserves both bindings in independent requests, inverse-to-property paths, excluded branch targets, alias-to-property branches, and duplicate-constrained multi-output requests.

## Remaining failures and interpretation

The only structured positive failures are the two “custodial accountability” owner requests, outside the frozen phrase grammar. Their plan diagnostics say unsupported; required owner evidence is already in the candidate pool. This demonstrates the cost of bounded-language recognition. They remain positive failures and are not excluded from recall.

The partial-dependency request emits the complete supported branch and reports execution `partial` because another dependency lacks its requested property. Gold contains only the available required evidence, so gold coverage is100% while execution remains partial. Do not equate gold all-required success with answering every requested branch.

For the duplicate-cap case, all four distinct required semantic assertions survive. Owner output retains both source provenance members while rendering one assertion. Alias-property cases retain each alias connector and its own target location, with complete provenance paths.

The exact unmatched `after` prohibition request reports `no_evidence`; the unsupported `before` qualifier reports `not_run` with unsupported parsing. Both abstain without confusing ordinary approval prerequisites with prohibitions. Source-temporal conditions remain independently enforced: later-learned Locker facts, expired Gate codes, and deleted Archive facts never enter output. Historical ownership uses the former owner, not current duplicates.

Old combined has4 forbidden query-budget assertions but zero temporal or scope violations. In this fixture forbidden includes query-incompatible literal prohibition facts, so reporting all4 as stale/security failures would be wrong. Defect and structured arms have zero forbidden, temporal, and scope counts.

## Limits

Curated structured facts, quoted entity names, explicit predicates, and supported grammar make this a mechanism experiment. It does not measure automatic extraction, unquoted arbitrary-name boundaries, general discourse resolution, downstream generated answers, or real-corpus task success. No embeddings or production daemon are involved. The next validation needs fresh natural-language tasks; these exposed cases must not become a new unseen holdout after repairs.

Parent continues report and delivery. Artifact: `/tmp/queryplan-results-independent.md`.
