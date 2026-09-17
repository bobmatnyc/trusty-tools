# Deterministic lexical fallback and passage experiment

Premeasurement protocol, 2026-09-17. Follow the real-memory validation with two targeted changes: lexical note support must not depend on typed graph parsing; packet allocation should preserve coherent exact-source passages. Keep embeddings, production changes and live memory writes excluded. Preserve all previous experiment inputs, policies and results.

## Fixed inputs

Reuse the strict read-only snapshot from the previous experiment (SHA256 `3d000743f7de080efb0b5c22ffc7231448f48e5fcd43918acea72a93a53d5166`). Its current time-eligible corpus contains 4,563 notes and 63,167 graph rows. No new export or model build is needed. This is historical requests against a fixed current corpus, not historical replay.

Choose a fresh 32 from the same 607 unique eligible project hook prompts, excluding all previous 32 exact texts. Sort by SHA256 of `memory-passages-v1`, NUL, and the exact prompt, then take the first 32. The selection occurred before implementation or relevance judgments. Sample SHA256: `4a707fa8309f029a763cbb0ea95de73539e8d34b0744e5ce8a6b4beab1e5610b`. The same scope, JSON hook unwrapping, markup exclusion and 5–200-word filter apply. No query rewriting or replacement in the sample itself.

Raw inputs, labels, packet text and per-query diagnostics remain outside Git under a mode-0700 private directory with mode-0600 files. Supply a private TMPDIR for temporary index files and the existing pinned offline tokenizer cache explicitly. Only generic code, hashes and aggregate evidence may enter the branch or ticket.

## Comparison contracts

The source-grounded research and interface specification must fix the arms, deterministic thresholds, candidate limits and passage algorithm before ranking. Include the unchanged raw BM25 control, lexical fallback over fixed chunks, and the same fallback with coherent passage packing; any additional control must isolate a stated confound. No post-holdout tuning or selection of only favorable metrics. Do not claim a graph ablation: the target is lexical selection and packet support, not new graph traversal.

All arms use 128/256-token budgets, the same native helper, exact source text and the same snapshot eligibility. Keep source identity, timestamps, scope, evidence offsets and output integrity verifiable. A changed passage span must have a real source locator; do not concatenate disjoint bytes into an invented contiguous quote. Report retrieval, selection and packing losses separately. Deleting metadata from answer text is not authorization to delete graph rows from the live store.

Gold is judged independently before rankings. Use source-note identities and exact UTF-8 byte intervals as the cross-arm authority, because chunk IDs and counts can change when segmentation changes. Primary support measures are complete required source-span coverage, union byte coverage, note recall and complete known-support sets; any chunk-group measure must disclose segmentation dependence. Empty positives score as misses. Ambiguous and unavailable requests are excluded from negative abstention. Known-support agreement is not comprehensive precision when labels are nonexhaustive. Report source-created-before versus source-created-after prompt strata without claiming creation means verification.

Run focused tests, strict type checking, independent code review, security review and input/gold integrity checks before official measurement. Keep timings separate from setup and validation; one warmup and three measured repetitions must give identical content signatures. Independently recompute results afterward without changing the frozen policy or labels. Preserve work on the existing research branch and update #8246; production integration remains a separate, backward-compatible rollout task.

## Pre-ranking label audit

The independent judge's first pass was reviewed by the parent before ranking. A transport-specific stale quote was narrowed to its explicit proxy-role statement; a publication-wave reference without a uniquely established referent became ambiguous; an old installation record could not answer a current-version question and became unavailable. Initial annotations were retained privately. The final labels contain 21 positives, 8 ambiguous, 2 unavailable and 1 memory-unneeded negative, with 29 mandatory source spans. Optional spans remain annotations and are excluded from primary completion.

Final gold SHA256: `016e8118d70a90206767f5ed1c444359f4dffaf7a873acb3299afdad912b7491`; prepared corpus/query SHA256: `fd3abfaffb613333295fa573bf641283f769ac855a69327102bfa9494fdacfb8`. Nineteen positive cases use notes created after their historical prompt; two use earlier notes. This limits the experiment to retrieval of known support from the fixed current corpus. It cannot establish historical availability, present-day truth, or reliable negative discrimination from a single negative case. The engineer does not receive private queries or gold before implementation and review.
