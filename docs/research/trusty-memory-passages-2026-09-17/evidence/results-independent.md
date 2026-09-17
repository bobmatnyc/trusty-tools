# Independent passage result audit

PASS for the recorded run-01 artifacts. Independently recomputed all 3,072 per-case support metric values with explicit byte-position sets, without importing the production scorer or its interval-merging function. All values and aggregate support means match. Verified 192 canonical packets and 8,396 recorded source spans. All 32 requests have identical native candidate lists across six arm/budget combinations and identical lexical seeds across four lexical combinations.

The audit checks prepared/gold hashes against provenance, original source metadata and body equality, exact UTF-8 slices, body digests, original fixed fact identities, derived passage digest identities, passage size bounds, emitted nonoverlap, expansion-byte arithmetic, passage seed attribution, native canonical rendering, independently counted offline cl100k_base tokens, and 128/256-token budgets. Three timing samples are present per case and each total equals the sum of its stages. The run does not save individual repetition packets, so repetition-content equality relies on the runner's enforced signature check, not an independent replay.

Implementation references: `experiments/trusty-memory-passages/passage_evaluate.py:91` defines the audited metrics; `:173` records stage spans; `:270` executes cases. Canonical formatting was reconstructed from `crates/trusty-memory/src/prompt_facts.rs:243`. Audit script: `/tmp/passage-results-audit.py`; aggregate data: `/tmp/passage-results-independent.json`.

## Results

Counts below are requests with any known-support byte / complete mandatory support, denominator 21 positives. Native candidates and lexical selected seeds do not vary with budget.

| Arm | Native candidates | Selected | Expanded | Emitted 128 | Emitted 256 |
| --- | --- | --- | --- | --- | --- |
| Raw BM25 | 14 / 2 | 14 / 2 | 14 / 2 | 7 / 1 | 12 / 1 |
| Lexical fixed | 14 / 2 | 12 / 2 | 12 / 2 | 7 / 1 | 12 / 2 |
| Lexical passages | 14 / 2 | 12 / 2 | 13 / 7 | 3 / 1 | 9 / 4 |

The shared retrieval list misses all labeled support on seven positives. Lexical selection reduces any-support coverage by two requests. Passage expansion makes seven complete sets available before packing; allocation retains one at 128 tokens and four at 256. Expanded coverage is potential adjacent-source coverage, not an improvement in native retrieval.

| Fixed → passage comparison | 128 tokens | 256 tokens |
| --- | --- | --- |
| Mean byte coverage | 10.06% → 9.03% | 22.88% → 32.34% |
| Byte coverage wins / ties / losses | 2 / 14 / 5 | 7 / 11 / 3 |
| Complete support wins / ties / losses | 0 / 21 / 0 | 3 / 17 / 1 |
| Note recall wins / ties / losses | 1 / 15 / 5 | 0 / 18 / 3 |

The 256-token complete-support gain is mixed: three requests improve, one regresses, and partial note recall decreases. Passage packing does not dominate fixed chunks. At 128 tokens it loses partial support without increasing complete requests.

## Temporal and label limits

Labels are 21 positive, eight ambiguous, two unavailable and one negative. Nineteen positives use notes created after the historical prompt; two use earlier notes. Every observed emitted support hit is in the later-note stratum. Both earlier-note positives have zero emitted support in every arm/budget. This is retrieval from a frozen current corpus, not historical replay or evidence of present-day truth. Creation time is not verification time.

Both lexical arms emit empty packets on the single negative; raw BM25 emits a packet. A denominator of one cannot establish reliable abstention. Positive empty packets are zero for raw, two for lexical fixed, and three/two for passages at 128/256. Gold is minimal and nonexhaustive. Optional annotations are excluded from primary metrics. Unlabeled output is unresolved relevance, not demonstrated error. This comparison does not isolate graph value.

## Verification

`/Users/masa/trusty-search-experiment/venv/bin/python /tmp/passage-results-audit.py` → exit 0.

Raw aggregate verdict: `{"status":"PASS","checks":{"span_checks":8396,"metric_checks":3072,"packets":192,"cross_arm_queries":32}}`.

Parent continues with the public research report and branch/ticket preservation. No inputs, labels, rankings, production source, Git state or live memory were changed by this audit.
