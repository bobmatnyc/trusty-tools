# Reading the ablation results

The six arms answer different questions. Compare every single intervention with the unchanged baseline on the same new fixture; compare the combined arm only as a package. Do not subtract separate effects and assume they add linearly.

| Comparison | What it can attribute | What it cannot establish |
|---|---|---|
| Selector vs baseline | Value and recall cost of claim eligibility by task intent, conservative lexical fallback, abstention and fact cap | General language understanding or calibrated probability of relevance |
| Relation graph vs baseline | Value of the complete relation policy with longest-name resolution, typed directed paths and changed seed/hop limits | Isolated benefit of graph storage or predicate postings alone |
| Claim index vs baseline | Effect of per-claim BM25 documents and fewer expanded candidate facts | Equal-work indexing comparison; claim and source candidate budgets differ |
| Cleanup vs baseline | Exact duplicate reduction, unique usefulness and token effects | Truth discovery, paraphrase consolidation or improvements caused by query intent |
| Combined vs baseline | End-to-end effect and remaining failures of the selected package | Contribution of each component without the single-intervention results |

Read candidate coverage, postselection coverage and final packet coverage separately. A candidate miss needs retrieval/indexing changes. A postselection loss needs intent or relevance-policy changes. A final-packet loss needs budget or support-group packing changes. Valid but irrelevant facts are selection errors; stale/scope errors are eligibility failures. An empty packet is correct only when the task lacks supported evidence. Report false abstentions on supported tasks.

Precision counts one useful semantic assertion once, even if several sources repeat it. All emitted assertions still consume denominator and token costs. Required groups can accept alternative provenance, while distinct conflicting values stay separate. Temporal equivalence is stricter than identical wording.

Maintenance results answer whether derived data can be upgraded safely and incrementally in this prototype. They do not prove that production APIs publish incremental native indexes, that all maintenance work is bounded, or that an installed daemon is backward compatible. Keep the existing source path usable when derived records are absent or incomplete, and preserve fact clocks during consolidation.
