# Lexical fallback and coherent passages: Stage 1 research

Status: research handoff, 2026-09-17. Scope: a bounded offline experiment in `/Users/masa/trusty-search-experiment/worktree`, using the same private frozen snapshot and a fresh independently selected set of 32 prompts. No embeddings, live writes, production integration, predicate normalization, or policy tuning against either held-out sample. References below are relative to that worktree. No private prompts, notes, gold labels, or rankings were read or executed for this research.

## Existing behavior and motivation

The prior report identifies two distinct losses: the typed policies discard all note support during selection; fixed chunks lose context during packing (`docs/research/trusty-memory-real-validation-2026-09-17/report.md:20-35`). The report proposes lexical selection and coherent passages before graph work. These are hypotheses for the fresh sample, not established improvements. BM25 also failed all three negative abstentions in that sample (report:11-18).

## Reuse boundaries

| Existing component | Contract and reuse |
| --- | --- |
| `experiments/trusty-memory-real-validation/real_adapter.py:55-75`, `:102-164` | `read_snapshot` validates version, counts, duplicate keys and row bounds. `adapt_snapshot` filters drawer expiry and graph intervals using the captured millisecond clock. Reuse unchanged; derive passage views from returned note bodies. |
| `real_adapter.py:77-100`, `:135-144` | `chunks` returns exact UTF-8 byte spans capped at 80 tokens; fact IDs are ordinal chunk numbers. Source IDs derive from drawer keys. Preserve these records for the fixed-chunk controls. Never relabel their IDs to imply unchanged chunk contents. |
| `real_adapter.py:36-53`, `:166-194` | Private directories/files use exclusive creation outside Git, permissions 0700/0600, and frozen-sample SHA checks. Reuse these writers and sample validation. |
| `experiments/trusty-memory-relevance/contracts.py:9-18`, `:46-58` | `Task` exposes prompt/scope/as_of/knowledge_cutoff only. `CandidateSet` and `Selection` carry evidence, support groups, diagnostics and rejections. Keep labels and evaluator IDs out of retrieval inputs. |
| `experiments/trusty-memory-query-plan/plan_index.py:22-34` | `build_scoped_index` owns scratch cleanup. `validate_context` rejects unknown or mismatched scope/time before retrieval. Retain equivalent checks for a new lexical projection. |
| `experiments/trusty-memory-prompt-enrichment/adapters.py:18-35` | `RustHelper` is a JSONL child process. Reuse the frozen binary and its request API; do not instantiate the embedding adapter imported by adjacent code. |
| `experiments/trusty-memory-relevance/relevance_index.py:63-77` | Existing native load requests take projection ID, documents `{id,text}`, triples, scratch directory. Distinct derived projections need distinct IDs incorporating corpus, policy and context hashes. |
| `experiments/trusty-memory-real-validation/real_evaluate.py:28-67` | Native search requests use `{op:search, projection, text, limit:20}`; cases separate candidate, selection and packet IDs, run a warmup plus three repetitions, compare deterministic signatures, and validate packets outside timing. Reuse this methodology. |
| `experiments/trusty-memory-prompt-enrichment/retrieval.py:93-95`, `:105-165` | `format_claims` produces canonical helper rendering. `pack` admits whole facts only and counts the complete rendered packet. Never estimate fit by adding per-span token counts. |
| `experiments/trusty-memory-prompt-enrichment/packet_integrity.py:10-28`, `:31-90` | Validator reconstructs authoritative facts, checks exact bytes/fingerprints, and reconstructs packet text. A query-created passage is not a registered old fact; it cannot be passed under the old ID. |
| `real_evaluate.py:125-193` | `merge_intervals` and byte intersections already supply chunk-independent coverage. Current required groups and `complete` use evidence IDs and must not be reused as the primary comparison after rechunking. |

## Minimum new implementation

Use one new experiment directory with two source modules and one synthetic test module: `passage_policy.py` for derived boundaries, lexical queries, deterministic selection and coherent packing; `passage_evaluate.py` for the new span gold contract, arm orchestration and summaries; `test_passage.py` for invented fixtures. Keep types near their consumers. Add a third source module only if the measured project line cap requires a split. Reuse immutable sibling imports through the existing bridge pattern; do not monkey-patch previous globals or rewrite old results.

The architect should settle a single frozen policy and numeric limits before fresh labels or rankings are visible to the implementer. A compact recommended arm sequence is: unchanged raw BM25 control; lexical note selection with old fixed chunks; identical lexical selection plus coherent passage packing. Keep query extraction, candidate cap and abstention identical in the last two arms to isolate packing. If a new passage index also changes candidate generation, name that difference explicitly or add a separate ablation; do not attribute the combined gain solely to packing. Old graph policies can remain a historical reference unless the parent requires fresh baselines.

## Query and selection contract

Input is `Task` plus eligible derived note records and frozen policy only. Preserve original prompt verbatim in private provenance. Output a deterministic lexical query representation with exact extracted anchors, ordinary terms, any truncation count, selected evidence and reason codes.

Exact paths, issue references, qualified symbols, quoted identifiers and names must survive extraction intact. Keep a bounded original-query search lane so unrecognized names are not silently dropped. A second compact lane may preserve technical anchors and informative terms in stable occurrence order. Any case-folded/tokenized matching representation is derived data, never replacement source text. Deduplicate queries and hits deterministically. Freeze the term cap, candidate cap, matching tokenizer and tie break before evaluation. Overlong input must produce explicit bounded/truncated diagnostics; it must not quietly discard the final path or issue identifier.

Selection must allow lexical note evidence when the typed parser is unsupported; parser readiness is not a precondition. Selection must not require drawer subjects such as `drawer:<key>` to appear in the prompt. This requirement avoids the subject equality gate at `experiments/trusty-memory-relevance/relevance.py:65-74`.

Abstention must be a deterministic predicate of prompt and eligible note text, not gold status, query ID, timestamps of the historical prompt, or manually recognized evaluation examples. Define a frozen informative-term/anchor support threshold and reject zero or generic-only overlap. Native rank alone is not calibrated relevance. Record `empty_query`, `no_candidates`, `below_threshold` and `budget` separately. A short follow-up without recoverable terms may correctly remain uncertain; do not invent conversation history. Explicitly report positive misses and negative abstentions independently.

## Passage and packet contract

A passage is an exact contiguous byte interval in one eligible drawer source, with source key, revision/body digest, start/end offsets, boundary-policy version, seed spans and stable identity derived from those fields. Keep source body, raw metadata and source clock fields unchanged. No concatenated fragments may claim one contiguous span.

Boundaries should respect paragraph/section breaks, list items and fenced command blocks. Sentence boundaries may split an oversized prose unit. Adjacent context expansion must have fixed byte/token/neighbor limits, stay in the same source and section, and include the lexical seed. Track seed candidates separately from expanded candidates: neighbors acquired during packing are additional evidence. Whole eligible notes must not enter candidate coverage merely because one seed matched.

Prefer precomputed bounded passage Facts registered in a derived Source view, which allows reuse of the canonical `Packet` and validator. These views keep the original body and source metadata but necessarily have different fingerprints because `Source.fingerprint` hashes its facts (`records.py:94-99`). Record the original source digest separately; never compare derived and original fingerprints as if identical. Query-time spans instead require a small explicit passage validator and canonical renderer; do not weaken the old validator to admit unregistered facts.

Pack an entire chosen passage or a predefined smaller coherent unit. Do not truncate bytes, split a UTF-8 code point, omit internal text while preserving the outer offsets, or silently cut a command. Define oversized atomic-block behavior explicitly (prefer skip with reason). Overlapping windows must not render repeated text; merge only when the exact contiguous union is registered/validated and fits, otherwise choose one deterministically. A merged passage must not add unbounded intervening text. Keep evidence order stable and source-local order within a group. Count headings, IDs and separators against 128/256-token budgets using the frozen offline encoding and `disallowed_special=()`.

## Chunk-independent judgments and metrics

Freeze gold against original note bytes, never candidate IDs. Each required support group contains one or more alternative bundles; each bundle contains one or more `(source_id, start_byte, end_byte, source_body_sha256)` spans that must all be covered. A group is complete when at least one bundle is fully covered by the union of returned intervals. All groups complete means complete known support. A simpler one-span-per-group schema is acceptable only if the judge does not need multi-span conjunctions or alternatives; its semantics must be explicit before annotation.

Validate span bounds, exact UTF-8 boundaries, original body digest, eligible drawer kind, unique query IDs and all 32 judgments. Require nonempty groups for positives and none for negative/unavailable/ambiguous statuses. Any supporting quote included by the judge must equal the byte slice. Alternatives must be explicitly labeled, never inferred from lexical similarity. A one-byte overlap may contribute fractional byte coverage but must not count as completed support.

Compute candidate, selected and emitted union-byte coverage; full-span/group coverage; complete support; note recall; packet tokens and abstention status. Union within each source before summing to avoid overlapping quotes or windows inflating credit. Do not average alternative bundles into a denominator that penalizes selecting one valid alternative; freeze a documented alternative-aware metric. Retain union coverage over all annotated support as a separately labeled diagnostic if desired. Never credit graph metadata as note support without independent corroborating note spans. Unlabeled emitted text is unresolved relevance, not proven wrong. Negative abstention has negatives only as its denominator; ambiguous/unavailable remain excluded as in `real_evaluate.py:195-223`.

## Time, provenance and verification

The unchanged adapter filters drawer expiry at `now >= expiry` and graph validity at `[valid_from,valid_to)` (`real_adapter.py:111-126`). It assigns snapshot time to adapted source observation/fact validity and preserves original record timestamps in metadata (`:136-144`). Do not reinterpret these as verified-at times or switch to historical-prompt replay. Derived indexes and packing must use the same captured snapshot context in every arm. Preserve explicit deletion/supersession behavior of `eligible_facts` (`records.py:232-243`) and do not infer supersession from similarity.

Freeze policy/code/helper/encoding/snapshot/sample/gold hashes. The parent owns fresh sampling and independent span annotation. Ensure no previous-sample prompt text is reused, and report sampling exclusions. Keep all content-bearing results outside Git. Public output is aggregates, hashes and generic code only. Record one-time index costs separately from query latency, hash repeated outputs excluding timing fields, and validate every final packet outside the measured interval.

Synthetic acceptance fixtures must cover: multibyte text and emoji boundaries; CRLF and blank lines; sentence punctuation in paths/versions; fenced commands and oversized fences; adjacent/overlapping passages; exact and one-token-over budgets with rendering overhead; identifier-only and generic-only queries; long prompts with anchors at the end; stable ties; no matching evidence; scope/time mismatch; exact expiry boundary; changed body/revision; missing source; spoofed fact/offset/text; duplicate evidence; overlapping gold; gaps between returned spans; alternatives and multi-span groups; and nonpositive labels excluded from support denominators.

Stage 1 is complete when the parent accepts this source-grounded scope. Stage 2 should finalize signatures, span-group schema and frozen numeric policy before implementation. No retrieval quality claim or production readiness claim is established by this research.
