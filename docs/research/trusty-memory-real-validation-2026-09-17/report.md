# Frozen memory policy on real project requests

The synthetic improvement did not generalize to this real sample. Both the previous combined policy and the structured policy returned empty packets for all 32 requests, including all 13 requests with independently identified useful note support. Plain BM25 returned some known support for 5/13 at 128 tokens and 7/13 at 256, but also injected content into every memory-unneeded request. No embeddings were used and no policy was tuned after these results.

## Comparison

“Complete” below means all labeled supporting chunks for a useful subtask, not correctness of an entire response or execution of the user's task. Gold is a minimal known-support set, not exhaustive relevance annotation.

| Arm | Token budget | Positives with any known support | Complete support | Required chunk groups recovered | Empty on memory-unneeded requests | Mean packet tokens | p50 / p95 ms |
|---|---:|---:|---:|---:|---:|---:|---:|
| Plain BM25 | 128 | 5/13 | 1/13 | 5/27 | 0/3 | 104.88 | 37.75 / 110.55 |
| Plain BM25 | 256 | 7/13 | 1/13 | 7/27 | 0/3 | 238.09 | 38.77 / 111.33 |
| Previous combined | 128 | 0/13 | 0/13 | 0/27 | 3/3 | 0 | 251.36 / 1236.98 |
| Previous combined | 256 | 0/13 | 0/13 | 0/27 | 3/3 | 0 | 249.99 / 1226.65 |
| Structured plan | 128 | 0/13 | 0/13 | 0/27 | 3/3 | 0 | 37.45 / 111.21 |
| Structured plan | 256 | 0/13 | 0/13 | 0/27 | 3/3 | 0 | 37.71 / 113.35 |

The three correct negative abstentions do not compensate for thirteen missed positives. The other sixteen requests—fourteen ambiguous and two without established support—are excluded from relevance and abstention claims. Larger packets increased BM25's partial support but did not increase complete support.

All arms retrieved the same candidate lists on this sample. They contained 12/27 required groups and complete support for 3/13 positives; macro candidate-group coverage was 47.44%. The graph policies lost all known support at selection. BM25 preserved candidates through selection, then lost support during packing: macro final group coverage was 21.79% and 28.21%; union supporting-byte coverage was 18.05% and 19.81%. This separates candidate retrieval loss, selection loss and packet loss.

## Why the graph did not help

The structured parser produced zero ready requests across 32 prompts; all 62 parsed request fragments were unsupported. Some entity roots resolved, but that did not create an executable request. The frozen grammar was designed for typed relation questions; this sample contains natural project tasks and follow-ups. The previous selector also rejects opaque note-text facts. This demonstrates an interface mismatch between these experimental policies and the current memory corpus, not that stored notes are useless.

The time-eligible corpus contains 4,563 notes, split into 24,854 exact 80-token-or-smaller chunks, and 63,167 graph records. Of those graph records, 62,294 (98.62%) are tags, mentions or containment links. Only 157 (0.25%) use predicates understood by the frozen structured policy: 146 `uses` and 11 `is_alias_for`. Native `is-a`, `depends-on` and `works-at` spellings were preserved rather than rewritten to make this test succeed. Only 191 objects exactly matched an eligible graph subject, the conservative adapter's entity-link rule.

No graph assertion received independently corroborated task-relevance credit. BM25 nevertheless packed six graph records at 128 tokens and twelve at 256 across the sample; their support remains unresolved, rather than being counted as semantically wrong. All graph rows have a provenance field, but that alone does not establish an originating note and exact supporting span. The graph may still be valuable as a bounded route from an entity or tag to supporting notes. This experiment does not establish that value, and it is not a clean graph-on/off ablation.

## Query, storage, or cleanup?

The observed failures support this implementation order, to be tested on a new sample rather than tuned against these labels:

1. **Query and selection:** allow grounded lexical note evidence to survive even when typed graph parsing is unsupported. Preserve exact paths, issue identifiers and names when deterministically extracting retrieval terms from a long task. Calibrate relevance/abstention separately; blindly injecting BM25 hits failed all three negative cases here.
2. **Chunking and packing:** build coherent evidence units around sentences, sections and commands, with neighboring context recoverable from exact source offsets. Fixed 80-token cuts made one useful passage require several independently retrieved chunks. Rank and pack supporting passages together within budget rather than assuming one matching chunk answers the request.
3. **Graph representation:** keep metadata navigation separate from asserted semantic facts. Normalize only explicit known predicate aliases in a versioned derived index, retain native predicates, and attach source-note/span provenance. Evaluate bounded graph-to-note enrichment with identical lexical seeds against a lexical-only control. A larger undifferentiated graph is not supported by these results.
4. **Deterministic consolidation:** refresh derived lexical fields, chunk boundaries, exact duplicate groups, predicate aliases and provenance links during dream cycles. Apply expiry and explicitly established supersession; do not delete valid tags just because they are poor answer text. Preserve fact creation, consolidation, index refresh and verification as different clocks. Existing records should remain readable, with gains accruing incrementally from refreshed derived data.

These are proposed follow-up changes, not implemented production behavior. A future graph ablation should hold query interpretation, lexical candidates and packing constant and measure additional corroborated note support per token and latency cost.

## Temporal and sampling limits

The sample was fixed before labels or rankings: 32 exact historical UserPromptSubmit texts chosen deterministically from 607 unique eligible project prompts. It includes ordinary action requests, not just memory questions. Retained logs, markup/length exclusions, disabled logging and timeouts limit representativeness; the texts are hook prompts, not exact historical recall API queries.

This uses a current snapshot, not historical replay. Seven of the thirteen positive cases use supporting notes created after their historical prompt. At 256 tokens BM25 found some known support in 6/7 of those cases, versus 1/6 among earlier-support cases; complete support was 0/7 and 1/6 respectively. Both graph policies returned none in either group. At 128 tokens the later-support partial count was 4/7. Creation before a prompt still does not prove availability, validity or verification at that time. The stored historical assertions were judged useful context, not independently verified current facts.

The judge selected minimal direct note passages without seeing rankings; the parent inspected every supporting quote and rationale. Alternative useful notes may remain unlabeled. Thus known-support overlap is a bounded recall diagnostic, not comprehensive precision. Thirteen positives and three negatives are too few to support a deployment claim.

## Operational result and artifacts

A strict read-only export decoded all 4,821 drawers and 86,658 graph records, including 1,277 older-format drawers, without silent losses. Eligibility excluded 258 expired drawers and 23,491 out-of-interval graph records. The exporter supports the four known drawer layouts and rejects trailing/unknown bytes. No live migration, dream cycle, full reindex, daemon restart or installation occurred. Production behavior remains unchanged; this does not yet prove a production incremental rollout.

One shared index build took 13.22 seconds outside query timing. There were 192 query/arm/budget cases, one warmup and three measured repetitions each. Repeated result signatures matched; canonical packet checks verified source spans, rendering and token budgets outside the timed interval. The debug native helper is byte-identical to the prior synthetic experiment. Timings were collected on a shared local machine, not an installed-daemon performance benchmark. The cold Rust exporter build took 35m33s, largely dependency compilation; it is not retrieval latency.

Raw prompts, notes, judgments, graph values and per-query packets remain outside Git in a restricted local directory. Public artifacts contain generic code, hashes and aggregates only. See [aggregate results](evidence/summary.json), [input audit](evidence/input-audit.json), [verification](verification.md), [protocol](protocol.md), and [independent audit](evidence/results-independent.md). All 145 previous experiment artifacts remain unchanged. Work is preserved on `codex/search-context-experiment`; production integration remains open under [#8246](https://github.com/bobmatnyc/trusty-tools/issues/8246).
