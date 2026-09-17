# Correctness amendment: chunk occurrence bounds

The first complete run is preserved under `experiments/trusty-memory-deterministic/results/run-01` but is superseded for choosing a chunking policy.

Its chunker counted the output of the shared BM25 tokenizer over the whole body. That tokenizer deduplicates terms. Consequently a 669-word manual with repeated vocabulary stayed below the nominal 128-token bound, and the chunking ablation did not actually split it. Identical snapshot sizes across the tuning grid exposed the issue. This is a chunk-bound correctness defect, not evidence that chunking cannot help.

Derived policy v2 counts lexical token occurrences: sum the shared tokenizer's output length per whitespace-delimited run, preserving repetitions between runs. It also caps a child at 4096 UTF-8 bytes, splitting oversized runs at scalar boundaries. The BM25 scoring implementation, fixture, labels, query split, and seven-policy tuning grid remain unchanged.

All treatments and tuning cells are rerun from scratch. Run-01 already exposed the held-out results; subsequent held-out results are explicitly a re-evaluation after a correctness fix, not a newly unseen test set. No labels or relevance-driven weights are adjusted in this amendment. Both runs and source hashes are retained. The repeated-vocabulary and large-single-run cases receive regression tests.
