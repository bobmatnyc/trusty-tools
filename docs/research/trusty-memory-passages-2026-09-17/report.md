# Lexical fallback and coherent passages on real memory

Coherent passages recover more complete support at 256 tokens, but they are not a general replacement for fixed chunks. Against the identical lexical seeds, complete known support rises from 2/21 to 4/21 and mean supporting-byte coverage from 22.88% to 32.34%. The number of requests receiving any known support falls from 12/21 to 9/21. At 128 tokens, passages reduce byte coverage and any-support recall without improving complete support. Keep this policy experimental.

The deterministic lexical selector avoids the earlier typed-parser gate and returns useful note support, while abstaining on the one memory-unneeded request. That single negative cannot establish safe abstention. No embeddings were used. This round changes only offline experiment code; the production daemon, live memory and previous experiments are unchanged.

## Head-to-head results

All three arms use the exact same original prompts and native mixed top-20 fixed-chunk candidates. Lexical fixed filters graph answer rows, applies a frozen lexical threshold and caps seeds at eight. Lexical passages uses the exact same seed IDs and order, then expands and packs registered source spans. Raw-to-lexical bundles selection changes; fixed-to-passages isolates the allocation/expansion policy, including its different locator overhead. It does not measure a new passage retrieval index or graph traversal.

“Complete” means all mandatory known-support spans for a useful subtask, not a correct full answer. Byte coverage is averaged over all 21 positives, including empty misses. Labels are minimal and nonexhaustive.

| Arm | Budget | Any known support | Complete support | Mean supporting-byte coverage | Empty on negative | Mean packet tokens | p50 / p95 ms |
|---|---:|---:|---:|---:|---:|---:|---:|
| Raw BM25 | 128 | 7/21 | 1/21 | 10.06% | 0/1 | 101.97 | 30.62 / 64.47 |
| Raw BM25 | 256 | 12/21 | 1/21 | 18.70% | 0/1 | 243.59 | 32.26 / 65.25 |
| Lexical fixed | 128 | 7/21 | 1/21 | 10.06% | 1/1 | 78.06 | 41.20 / 126.31 |
| Lexical fixed | 256 | 12/21 | 2/21 | 22.88% | 1/1 | 162.03 | 43.73 / 127.57 |
| Lexical passages | 128 | 3/21 | 1/21 | 9.03% | 1/1 | 88.59 | 41.69 / 131.81 |
| Lexical passages | 256 | 9/21 | 4/21 | 32.34% | 1/1 | 189.25 | 43.31 / 128.43 |

At 256 tokens the paired byte-coverage comparison has seven wins, eleven ties and three losses. Complete support has three wins, seventeen ties and one loss. At 128 tokens byte coverage has two wins, fourteen ties and five losses; complete support ties on every request. The aggregate gain therefore hides meaningful regressions. Passage note recall also falls at both budgets.

## Where support is lost

| Stage | Positives with any support | Complete support | Mean byte coverage |
|---|---:|---:|---:|
| Native candidates, all arms | 14/21 | 2/21 | 25.08% |
| Lexically selected fixed seeds | 12/21 | 2/21 | 22.88% |
| Reachable passage spans before budget | 13/21 | 7/21 | 50.21% |
| Passage packet, 128 tokens | 3/21 | 1/21 | 9.03% |
| Passage packet, 256 tokens | 9/21 | 4/21 | 32.34% |

Adjacent context creates real additional support: the expansion stage reaches five more complete support sets than the selected fixed seeds. This is packing-time expansion, not improved native candidate retrieval. Budget allocation then discards much of that support, especially at 128 tokens. Larger early passages and locator overhead compete with later seeds. These results motivate testing a seed-first, budget-aware allocation policy; they do not establish that a particular replacement algorithm will win.

Candidate retrieval remains a separate bottleneck: seven positives have no known-support byte in the native candidates. The lexical gate loses known support on two more positives. There are two empty positive packets for lexical fixed at both budgets; passages adds one budget-only empty positive at 128. The selector abstains on six of all 32 requests. Unlabeled output on ambiguous/unavailable requests remains unresolved relevance, not proven wrong.

## Representation, graph and timing

The same eligible corpus contains 4,563 notes and 63,167 graph rows. Derived passage windows represent 97.77% of note bytes and at least some text in 4,561 notes. There are 252 oversized units across 237 notes, accounting for 145,224 skipped bytes; no oversized fenced block occurs in this snapshot. Small blank gaps and window boundaries explain why full-note representation is lower than any-note representation. Atomic command blocks remain unsplit.

Raw BM25 emits three graph rows at 128 tokens and two at 256 across this sample. Neither lexical arm emits graph rows, and no graph assertion received corroborated support credit. This is not a KG-on/off ablation: the graph still shares the candidate pool, and lexical selection changes more than graph eligibility. The original graph-to-note enrichment premise remains unproven.

One shared native index build took 13.40 seconds, lexical statistics 0.40 seconds and passage derivation 3.17 seconds, all outside query timing. Lexical selection adds a tail: its p95 is about 70 ms, versus under 0.004 ms for raw selection. Exact-anchor frequency checking scans eligible note text in the current experiment; a versioned derived lookup/cache is a concrete optimization candidate. Passage packing itself has p50 about 1.8–1.9 ms. Timings use a debug helper on a shared machine, in fixed arm order, and exclude validation; they are not installed-daemon performance guarantees.

## Next fixes and compatibility

1. Test budget-aware passage allocation with minimal coherent seed units first, then neighboring context from remaining tokens. Preserve exact source offsets and indivisible commands. Require paired support gains at both budgets and inspect regressions, rather than choosing a winner from one aggregate metric.
2. Test compact deterministic query terms and exact identifiers as an additional bounded retrieval lane. Keep the original prompt lane and identical packing controls. Improve candidate recall separately from selection and packing.
3. Precompute or cache anchor lookups and passage boundaries in versioned derived state. Dream-cycle consolidation can refresh changed records deterministically and invalidate entries on revision, expiry or explicit supersession. Keep source creation, verification, consolidation and index refresh as separate clocks.
4. Once lexical support is stable, test bounded graph-to-note enrichment with identical seeds and packing, requiring originating note/span provenance. Metadata links should route to evidence rather than consume answer space as unsupported assertions.

These are follow-up hypotheses, not changes tuned against this holdout. Future production integration should preserve old records and API behavior, fall back to existing retrieval when derived state is absent, and refresh derived entries incrementally. No full reindex should be required to continue operating; refreshed entries can provide gains over time. This experiment does not yet prove that production migration path.

## Evidence and limits

The fresh deterministic 32-request sample shares no exact prompt with the previous sample. Gold was frozen independently before rankings: 21 positive, eight ambiguous, two unavailable and one negative, with 29 mandatory spans. Optional alternatives are excluded from the primary metric. The sample uses historical prompts against a fixed current snapshot. Nineteen positives use later-created support; the two earlier-support positives receive no known-support bytes in any emitted packet. All observed support gains occur in the later-note stratum. This is not historical replay, present-truth verification or a deployment-quality accuracy estimate.

The run completed 192 query/arm/budget cases with one warmup and three measured repetitions each. Content signatures matched. Independent byte-set arithmetic checked 8,396 spans and 3,072 metric values, candidate/seed identity and all 192 packets. All 23 measured Python module hashes and 391 previous artifacts remained unchanged. Runtime policy and labels were not tuned after measurement; the only concurrent edit strengthened a synthetic regression test without changing runtime code.

See [protocol](protocol.md), [aggregate results](evidence/summary.json), [independent audit](evidence/results-independent.md), [independent aggregates](evidence/results-independent.json), [provenance](evidence/provenance.json) and [verification](verification.md). Raw prompts, notes, graph values, gold and packet text remain private outside Git. The experiment is preserved on `codex/search-context-experiment`; production integration remains open under [#8246](https://github.com/bobmatnyc/trusty-tools/issues/8246).
