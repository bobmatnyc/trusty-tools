# Frozen lexical and graph retrieval benchmark

Snapshot: `90b6aeb944e1d010f3690281ec24efe7b89435c3`.
Corpus root: `/Users/masa/trusty-search-experiment/worktree`.
Dataset: `queries.json`; SHA-256 `0dd48ce4af883fb38e3918689c10b018855b265076af6896228ee2559bfdf582`.

The 48 queries contain 12 exact-symbol lookups, 12 behavior questions, 12 code-relationship questions, and 12 documentation questions. Expected source locations cover 8 crates: trusty-analyze, trusty-channels, trusty-common, trusty-installer, trusty-memory, trusty-publish-guard, trusty-review, trusty-search. Documentation includes crate guides, workspace policy, and an ADR. Embeddings are outside this experiment.

Each type contributes eight tuning queries and four held-out queries. Within each type, sort rows by SHA-256 of `trusty-chunk-bm25-kg-v1:` plus the row ID. Assign the first four to `held_out`, the remaining eight to `tuning`. This rule was fixed before retrieval runs. Do not select parameters from held-out results; run those after selecting a configuration on tuning data.

## Label evidence and scoring

Labels were derived from direct source reads, not retrieved search hits. Every code target identifies its declaration line and symbol. The one-line interval anchors the owning chunk even if chunk boundaries change. Documentation intervals identify the answer-bearing section. The JSON was validated against the snapshot: all paths, line bounds, function declarations, and graph callsite references resolve.

Graph rows contain `kg_seed` with path, symbol, direction, and a source callsite note. Six ask for callees and six for callers. Each expected item is a relevant direct endpoint, not a complete adjacency list. Seeded graph lookup and natural-language lexical retrieval are distinct tests: report their results separately. A seeded graph experiment is an oracle-seed diagnostic and does not establish end-to-end entity resolution quality.

For code retrieval, require normalized source path plus overlap with the declaration line. For documentation, require path plus interval overlap; consider manual review for very broad chunks. Do not count a file match alone as a chunk-level success. Report Hit@1/5/10, MRR@10, returned bytes/tokens, and latency by query type and split. Recall over these partial labels means recovery of the labeled targets only, not exhaustive relevance recall. Multiple chunks from one expected symbol should not count as multiple successes.

## Limits

This is a small, manually authored developer benchmark rather than a representative production query log. Functions with clear behavior were selected for reliable labels. The source corpus is the whole repository, but labels sample eight crates and documentation, with extra coverage of search, shared utilities, and installer behavior. It does not test all languages or every crate. The split is stratified by query type, not by file or subsystem, so it measures new questions about a familiar repository, not generalization to unseen repositories. Behavior queries may retain domain vocabulary from source. Graph edges are direct static calls; dynamic dispatch and runtime-only relationships are not covered.

No retrieval scores were consulted during authoring. Reusing held-out outcomes to choose another configuration converts that split into tuning data; create a fresh held-out set before making another generalization claim.
