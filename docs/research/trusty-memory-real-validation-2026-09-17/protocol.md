# Real-memory validation protocol

Premeasurement protocol, 2026-09-17. Validate the immutable query-plan policy on actual project hook prompts and a current memory snapshot. Embeddings are excluded. Existing experiments remain unchanged. Production integration stays open under #8246.

## Private inputs and sample

Use only the trusty-tools palace and its retained UserPromptSubmit logs. Decode a JSON hook envelope's string prompt; otherwise retain plain text. Exclude empty/nonstring prompts, text beginning with markup, and prompts outside 5–200 whitespace-separated words. Deduplicate exact strings, retaining the first event. Sort by SHA256 of the fixed memory-real-validation-v1 salt, NUL, and original prompt; select the first 32. Sampling occurred before policy outputs or source relevance judgments. This includes ordinary action requests, not just explicit memory questions.

Raw prompts, snapshots, judgments, emitted packets and source identifiers remain in a permission-restricted private directory outside Git. Commit only generic code and aggregate evidence. Do not post private text to tickets. Source hashes and anonymous case numbers may support reproducibility without publishing contents.

The sample contains 32 of 607 unique eligible prompts from 671 eligible rows among 6647 project hook records. Exclusions: 5641 markup-start records, 332 outside the length range, 3 empty/nonstring; 64 exact duplicates. Two malformed log lines across the scanned log files could not be scope-classified. Retention, disabled logging and timeout cancellation can omit requests; these logs also include completed empty enrichment results. They are historical hook prompts, not exact recall API query strings.

## Snapshot and temporal meaning

Export canonical drawers and graph rows using a hard read-only transaction. Preserve native predicates, source timestamps, graph validity and provenance; report decode errors rather than silently dropping rows. Do not instantiate a model, start/restart a daemon, run a dream cycle or modify live memory. Evaluate historical prompts against the current snapshot. Incomplete historical revisions mean this is not historical replay, and graph creation/validity is not proof a real-world claim was recently verified.

Drawer text is opaque exact-span evidence. Graph rows are their own graph-record evidence; rendered triples are not invented source-note excerpts. Inventory graph-to-drawer provenance and native predicate compatibility. No manual rewriting into the synthetic vocabulary, new grammar, or post-result tuning.

## Comparison and independent judgments

Compare plain native BM25, frozen old_combined and frozen structured_plan with the same scope/time eligible input, 128/256-token budgets and existing formatter. Report applicability across all 32 cases, plus candidate, selection and packet losses. Retrieval differs across arms, so do not claim a pure graph ablation. Keep drawer-versus-graph metrics separate.

Before viewing rankings, independently judge each prompt against snapshot content. Label positive direct support with exact supporting source spans; label explicit no-memory-needed requests only when clear; use unknown/context-dependent where absent conversational context or incomplete corpus prevents a defensible judgment. Never treat an unknown as a successful negative. Evaluate relevance only on defensible labels and report judgment coverage. Review judgments independently before scoring. Standing conventions are not automatic task-relevance credit.

Record deterministic outputs, timing separately from snapshot/index construction, and no model construction. Policy hashes stay fixed; private inputs and labels are frozen before ranking. Any limitations or failure of the synthetic result to generalize are outcomes to report, not grounds to rewrite this holdout.

## Premeasurement review corrections

Independent review identified mixed drawer/uncorroborated-graph precision denominators, missing required-group completion, and missing partial supporting-byte coverage. Correct these measures before ranking: score drawer relevance and corroborated graph precision separately, report unresolved graph evidence explicitly, validate required alternative groups, and measure union byte coverage without double counting. Every judged positive contributes to complete-query success or failure. These are evaluation repairs, not retrieval-policy changes.

The inherited read-only database fallback and search helper use temporary files. Run both with an explicit temporary directory inside the mode-0700 private evaluation root, record that boundary, and audit owned temporary residue afterward. The new exporter does not enforce this inherited temporary-file location itself; this experiment applies an operational mitigation without modifying production storage code.

Compilation can include embedding dependencies because the existing memory-core feature includes the embedder and bundled runtime. This does not authorize constructing a model: all comparisons exclude embeddings. The cold exporter build also compiles vendored OpenSSL; build time is excluded from retrieval latency. A storage-only feature split could reduce future build cost but is outside this experiment.

The first real export failed at the fourth drawer because the current postcard record decoder reached the end of a legacy record. It created no final snapshot and left the private temporary directory empty. Explicit compatibility decoding of known historical drawer layouts is permitted before retrying, with decoder-version counts and absent-field metadata. Unknown or malformed layouts must still fail. No writable live-store fallback is permitted.

Before rankings, the independent judge identified 13 positive requests, 3 memory-unneeded negatives, 14 ambiguous requests and 2 without established supporting evidence. The parent inspected every quoted supporting passage and rationale. Gold is a minimal known-support set, not an exhaustive labeling of every useful note: emitted overlap with this set must not be presented as comprehensive semantic precision. No graph assertion received corroborated relevance credit. Seven positive cases use supporting notes created after their historical prompt; report the six earlier-support and seven later-support cases separately as a descriptive check. Creation time is not verification time, and this stratification does not reconstruct an historical index.
