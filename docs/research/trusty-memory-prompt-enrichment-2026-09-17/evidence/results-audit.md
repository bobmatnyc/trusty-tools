# Independent results audit: resident prompt enrichment run-01

Read-only audit of frozen inputs and `experiments/trusty-memory-prompt-enrichment/results/run-01`, 2026-09-17. No ranking, embedding, tuning, input mutation, or Git operation. **PASS for reported numerical results, with the interpretation limits below.**

## Main results verified

Independently recomputed required-group coverage from packet evidence IDs and canonical gold. Inspected complete claim inclusion, source byte spans, revisions, and standing identities for seven treatments ×153 packets. All checks agree with stored metrics. Eight frozen manifest hashes match provenance.

| Treatment | Required coverage | All-required success | Macro fact F1 | p50 packet ms | p95 packet ms | Useful task facts/100 total tokens |
|---|---:|---:|---:|---:|---:|---:|
| BM25 | 73.6111% | 63.8889% | .217186 | 2.131667 | 3.155342 | .504406 |
| BM25+graph | 99.0741% | 97.2222% | .248979 | 2.621750 | 3.776550 | .593309 |
| BM25+dense | 96.1420% | 90.7407% | .247819 | 5.790792 | 9.494674 | .568134 |
| BM25+graph+dense | 98.7654% | 97.2222% | .235472 | 6.185833 | 10.131082 | .549762 |

Coverage is the macro mean over **108 answerable query-budget observations**, representing **36 unique heldout task questions ×3 budgets**. Per category, 9 observations represent 3 questions ×3 budgets. The remaining observations are 12 unique task-empty questions ×3 and 3 standing-only questions ×3. These are not 153 independent questions. Latency has 765 warm samples per treatment (153 packets ×5 repetitions), not 765 independent quality samples.

The useful-facts column is the macro mean across all153 packets. Its numerator excludes standing facts; its denominator includes full packet tokens, including standing prelude/headings. It must not be described as task-only token efficiency. Answerable-only macro values are .714576/.840521/.804856/.778829 respectively. The graph-only control reaches .623455 over all packets but loses coverage (65.2778%); usefulness per token alone is not sufficient.

Every audited packet has standing coverage1.0. All four main treatments have **0/36 task-empty packet successes**, across 12 unique negative questions. Scope, forbidden, and stale counters are zero. That means safety filters worked for these fixtures, while unrelated currently valid context still fills unsupported-task packets. Negative-task tokens total7246/7601/9631/9664 over36 observations for the four main treatments.

Stored aggregate precision includes negative-query observations; main values are .104347/.114075/.116357/.106771. Standing/current-policy controls report .25 because empty negative packets get precision1 while empty answerable packets get0. Do not present that control .25 as precision of emitted useful assertions: these controls contain no task-relevant evidence.

## What supports the graph premise

- Graph addition recovers exact and lexical categories from0 to100% coverage, one-hop from50 to100%, paraphrase from66.67 to100%, and two-hop from66.67 to88.89%.
- The selected graph policy is one seed/one entity hop. The hybrid's two-hop gain does **not** prove a standalone two-hop traversal succeeded. Graph-only two-hop coverage is33.33%; lexical complementary facts complete many hybrid packets.
- Graph-only packet p50 is .343791ms at65.2778% coverage. BM25+graph adds about .490ms to baseline median while increasing evidence coverage by25.463percentage points. Its graph stage median is24.083µs; packet formatting/packing is far more expensive (2.041625ms median).
- Dense retrieval also improves baseline; adding dense to graph yields slightly lower coverage/F1 and higher latency/tokens than graph+BM25 here. This supports graph-first enrichment for this workload, not a conclusion that embeddings never help.
- Existing newest-200 lexical graph policy control has0 task coverage. It intentionally meets the crowdout fixture and restricts predicates; this identifies a failure mechanism, not typical production quality.

## Scaling evidence and limits

At10,000 unrelated noise edges, total native graph size10,513 edges includes512 hub edges plus1 needle edge:

| Operation | Needle API µs | Needle end-to-end µs | Returned | Hub API µs | Hub end-to-end µs | Returned |
|---|---:|---:|---:|---:|---:|---:|
| Native subject query |47.875|102.583|1|1681.209|6872.917|512|
| Native adjacency |6.500|63.792|1|1212.417|6249.083|512|
| Bounded projected graph |4.042|4.083|1|10.625|10.708|32|
| BM25 |24.042|55.167|1|508.708|571.250|20|

Needle native adjacency API medians across100/1000/10000 noise edges are7.625/5.208/6.500µs, with a real nonempty1-edge result. This supports the quick indexed-neighborhood premise. The bounded projection scans2 needle facts and32 hub facts; no empty-response timing artifact.

Do not convert the hub table into an unconditional native-versus-projected speedup: output caps differ512 versus32, the projection is in-process Python, and native includes helper IPC/serialization in end-to-end values. Native APIs do not report examined edges (`-1`), so no measured native scan count exists. The scaling bounded query supplies an exact entity hint; it does not measure natural-language entity resolution.

The query benchmark uses18 precomputed scope/time/scenario projections. Reported projection construction sums1,171.603667ms, native disk sizes sum10,461,184bytes. Corpus encoding1,451.384167ms and model load86.652083ms occur outside timed queries. No source-model truncation events. Run peak RSS453,230,592bytes covers the process experiment, not isolated per-lane marginal cost. Unchanged maintenance records0 changed/0 removed with unchanged hashes.

## Remaining interpretation constraints

Curated synthetic facts and explicit relations are available to all source prose representations. Graph excludes structural tags while BM25 searches them; the result tests the predeclared improved graph policy, not graph data structure alone. Entity names appear in prompts, scenarios share templates, and the graph has accurate authored relationships. No real-corpus extractor, generated answer correctness, arbitrary-clock resident cache, installed daemon latency, or production compatibility is established.

The standing control benchmarks a scoped experimental prebuilt cache with public formatting, not the installed cross-palace cache's full behavior. Complete evidence claims are rendered in main packets; provenance exists in a sidecar outside the prompt token budget. Native controls use object-oriented public bullets, a different attribution representation. Native adjacency hydration and custom graph projections must remain separate in reporting.

Parent continues report and delivery. Audit artifact: `/tmp/graph-prompt-results-audit.md`.


## Draft report numerical check

Read `docs/research/trusty-memory-prompt-enrichment-2026-09-17/report.md`. Positive-only precision independently recomputes to 13.9129%, 15.2100%, 15.5142%, 14.2362%; the report rounding13.9/15.2/15.5/14.2 is correct. Positive-only useful-task-facts/full-packet-token ratios round .715/.841/.805/.779, matching the report. The report clearly identifies108 positive query-budget cases and distinguishes all-packet token means. Budget/category values and whole-source coverage match artifacts.

One wording clarification recommended: replace opening “Combining both additions did not improve over graph alone” with “Adding embeddings to BM25+graph did not improve it.” The graph-only control actually performs worse than the combined treatment; the intended comparison is BM25+graph. Also state explicitly that hybrid two-hop coverage comes from complementary lanes under a selected one-hop graph policy, not proof of a standalone two-hop graph traversal. No other numerical discrepancy found in the report.
