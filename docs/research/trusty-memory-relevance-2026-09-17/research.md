# Deterministic relevance experiment: reuse and independent interventions

Source-only stage1 review, 2026-09-17. Previous experiment stays immutable. All paths below are relative to `/Users/masa/trusty-search-experiment/worktree`. No execution of ranking, embeddings, or new code. Parent owns new fixture/protocol.

## Mechanisms to separate

Previous `experiments/trusty-memory-prompt-enrichment/retrieval.py:42-45` ranks source documents and expands **every eligible fact** from each source. RRF then ranks expanded fact identities (`:81-91`). It discards lexical scores as calibrated relevance evidence. `projection.py:164-174` emits adjacent assertions regardless of requested relation. `retrieval.py:131-151` fills the remaining budget with every fitting candidate, with no claim relevance threshold. These are three separable causes of irrelevant prompt enrichment.

## Minimal independent ablations

| Treatment | Change from fixed previous BM25+graph baseline | Must remain fixed |
|---|---|---|
| baseline | Previous source BM25 + generic graph, frozen selected one seed/one hop and RRF60 | Eligibility, sources, formatting, budgets, graph caps |
| selector | Postfusion claim selector + explicit abstention policy | Candidate generation, native source BM25, graph traversal |
| relation_graph | Replace graph lane with relation/direction-aware bounded traversal | Source BM25 lane, fusion, no postfusion selector |
| claim_bm25 | Native BM25 document per claim instead of per source | Generic graph, fusion, no intent gate/selector |
| consolidated_index | Incrementally materialized derived indexes with query policy identical to chosen control | Eligibility and relevance policy; this is equivalence/cost, not automatic quality gain |
| combined | Claim BM25 + relation graph + selector, each frozen independently | No retuning combined on heldout |

Add a direct-predicate-index control if affordable: exact scoped entity+predicate lookup often answers one-step questions without graph traversal. That distinguishes benefit from typed indexing versus graph expansion. It is a mechanism control, not another large tuning dimension.

## Reuse seams

- Reuse immutable source/fact/query/evidence types and exact-span contracts in `records.py:67-129`. Import from a deliberately isolated module path or copy into the new experiment with original hashes; do not modify prior files. Keep gold imports entirely evaluator-side (`records.py:245`).
- Reuse temporal/scope eligibility (`records.py:232-243`) and standing prelude/full-claim packer (`retrieval.py:105-164`), plus independent `packet_integrity.py` checks. Same-byte claims and sidecar source identity remain required.
- Reuse bounded graph projection shape (`projection.py:24-40`) and cap accounting (`:135-186`). Build a new direction-preserving adjacency keyed by `(scope,subject,predicate,direction)`. Current adjacency merges both directions; evidence triples retain direction, so traverse using original subject/object checks.
- Reuse native helper load/search/format operations: existing documents already accept arbitrary string IDs. For claim indexing, use `Evidence.id` as document ID and map hits directly to evidence rather than expanding `by_source`. No new production Rust scorer required. Native `BM25Index::upsert_document_reporting` / `remove_document` / `score_query_all` are available in `crates/trusty-common/src/bm25.rs:333`, `:380`, `:473` for incremental helper support if needed.
- Avoid constructing `LocalEncoder` or `build_source_index` (`projection.py:43`) because it eagerly encodes every source. An embedding-free source container and native lexical indexes suffice. Do not silently use fake vectors; no dense treatment runs.

## Selector contract

Pure `parse_intent(prompt) -> Intent` returns recognized relation sets, requested direction, entity mentions, and unsupported/ambiguous status. It sees only user query text and a frozen general vocabulary/rule table. It never reads category, query ID, expected facts, split, or source answers. Category is evaluation metadata.

Pure `select(intent, ranked_evidence, policy) -> Selection` returns selected IDs and reason-coded rejections. Minimum reasons: unknown_intent, unresolved_entity, ambiguous_entity, wrong_relation, no_complete_path, duplicate_assertion, below_threshold, and budget_limit. Keep score/reason diagnostics for every candidate. Unknown intent must follow an explicit predeclared policy: abstain, or conservative lexical evidence threshold. Always-abstain can game negatives, so report answerable recall jointly.

Requested multi-relation tasks use a union of relation demands. Do not stop at the first regex match. Conjunction requires evidence for each demand; distinguish partial supported packet from total abstention. Negation and hypothetical requests must not be classified by an isolated positive keyword. Requested “why” cannot be answered by an owner or contact assertion just because the entity matches.

Do not use absolute BM25 values as universal probabilities. For a selector-only ablation, any token-overlap, normalized score/margin, intent confidence or minimum support thresholds must be frozen and tuned on tune only. Multi-hop evidence may have zero direct query overlap; preserve connecting evidence when a supported relation path reaches the answer.

## Relation graph contract

Resolve longest exact boundary-matched entity mentions before shorter prefixes. Preserve ambiguous alias candidates; do not resolve “Harbor Twin” through the shorter “Harbor” alias. An explicit request to list alias alternatives may emit both alias assertions without choosing either target.

Compile supported relation requests to bounded paths: e.g. maintained_by → escalates_to → contact_window. Literal assertions on the reached entity do not consume relation hops, consistently with previous clarification. Every inspected relationship/property counts against scan budget. Preserve incoming/outgoing direction. Return the complete supporting path, not a detached target fact. Never invent paths for arbitrary predicates; unsupported path templates yield a reason-coded abstention or lexical fallback.

Changing predicate iteration to skip irrelevant edges via index avoids examining them; record both index probes and examined edges so this is not misreported as a cap improvement. Store deterministic sort/tie-break rules. Cap seeds, visited nodes, scanned assertions, emitted assertions, and tokens independently; retain truncation reasons.

## Claim indexing contract

Index text should be a fixed formula such as `source title + subject + predicate label + complete claim`, derived only from source metadata. Every treatment must have the same factual information; graph metadata cannot hide facts unavailable in prose. Specify whether repeated source titles are included because they can dominate every claim from a long mixed source. A clean initial ablation uses `source title + claim` and changes only document granularity.

Set claim candidate limit explicitly. Twenty sources can yield hundreds of candidate facts while twenty claims cannot; report this candidate-work difference, and optionally evaluate an equal candidate-fact budget. Keep graph fixed. A claim index alone does not imply task relevance or abstention.

## Consolidation is materialization, not truth discovery

Maintain per-source derived records and reverse dependencies: source digest/revision, policy version, claim identity, entity/alias/predicate postings, directed adjacency, native BM25 documents, and provenance members. A changed source deletes all obsolete derived IDs before atomically publishing replacements. Deletes and resume checkpoints must be idempotent; replaying the same event cannot resurrect data. A failed publication leaves the old coherent generation or an explicit missing-derived fallback.

Reuse staged update intent from `projection.py:49-78`, but source-copying and vector encoding are not a graph-index maintenance implementation. Old projection construction (`:92-132`) rebuilds entire scope/time snapshots; do not call that incremental publication. Report records examined as well as changed: copying/scanning all state inside a batch still costs O(N).

Exact duplicate canonicalization can reduce prompt repetition and index cost, but retain all source provenance. Never merge conflicting objects or distinct validity intervals. With dedup enabled, gold required groups must allow equivalent source evidence; score unique semantic assertions separately from provenance count. This is a cleanup quality intervention distinct from intent filtering and should have its own toggle/control if credited with quality gains.

Query eligibility remains time-sensitive even when no source changes. Handle valid-from/expiry/valid-to boundaries at read time or schedule derived expiration deterministically. Reads/consolidation must preserve observed_at and verified_at. Same source/time/policy input must yield identical logical indexes and packets after incremental versus clean rebuild. Missing derived records must use source retrieval without requiring a full rebuild.

## Fresh holdout design

Use different task structures, not renamed families. Suggested independent heldout compositions:

- A requested comparison of two entities, each with a different requested relation; lexical distractors mention both names.
- A long operational note mixing release gates, emergency contact, retention period, old owner and unrelated preferences; only two nonadjacent claims are required.
- A reverse dependency question with two incoming branches and a cycle, requiring complete attribution paths.
- A multi-step routing task where aliases resolve at an intermediate node and one branch expires exactly at query time.
- A same-name entity in two scopes, an ambiguous alias within one scope, and an explicit ambiguity-list question contrasted with an answer requiring disambiguation.
- An unsupported causal/motivation question whose entity has many true related facts; a supported “why” assertion in another task prevents a blanket question-word veto from winning.
- Exact duplicate assertions in multiple sources alongside same-predicate conflicting values and old/new revisions. Only duplicates may consolidate.
- Unknown intent phrasing, negated actions, multiple requested relations, and a deliberately missing relation in an otherwise well-connected graph.

Freeze query/gold/source and policy vocabulary before retrieval. Keep schema and predicate vocabulary shared as an interface, but do not author heldout prompts from selector regex literals. Measure entity resolution, intent recognition, candidate coverage before gating, postselection coverage/precision, abstention specificity and false-abstention rate, supporting-path completeness, duplicate content, task tokens, and packet latency separately.

## Risks requiring explicit constraints

`records.py:55-62` validates ISO strings but preserves raw forms; eligibility compares strings. Require a canonical fixed timestamp representation (or compare parsed instants) in new inputs. `records.py:116` projection keys omit policy/index generation; new keys must include these. Existing source BM25 indexes full bodies even if some contained facts are ineligible (`projection.py:118`), while emitted evidence is filtered. Freeze whether the baseline preserves that ranking behavior; do not silently change it in only one lane.

No new implementation or repo edits. Parent continues protocol/fixture design and implementation. Findings are source-derived, not a new benchmark result.
