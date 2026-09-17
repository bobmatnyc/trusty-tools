# Evaluation protocol

Frozen before measurements. Source snapshot: 90b6aeb944e1d010f3690281ec24efe7b89435c3. Dataset: 48 independently source-labelled queries, 32 tuning and 16 held-out. No embeddings or model-generated summaries.

## Treatments

Six fresh indexes: appended context budget 0/64/128 words crossed with oversized AST chunk windows 100/64 lines. The 0-word, 100-line treatment is baseline. All use the same compiled binary, source archive, no-vector guard, BM25 tokenizer/ranking and KG implementation. Each process uses its own marked corpus and data directory, explicit loopback endpoint, and rejecting embedder. Zero calls is mandatory.

## Measures

Primary retrieval comparison uses natural query text through lexical and graph stages. It measures success@1/5/10, reciprocal rank@10 and labelled-source recall@10. Matching requires path plus range overlap, never text from enrichment. The labels are incomplete relevance judgments; unlabelled results are not necessarily irrelevant. Graph questions are included in both lanes; optional oracle-seed diagnostics are reported separately.

Output measures compare serialized result arrays: full HTTP hits, the six-field MCP-equivalent compact view, and experimental cards. Tokens use cl100k_base. JSON framing metadata is excluded equally. Cards must preserve ranking and source identity. These are payload measures, not proof of agent task completion or comprehension.

Latency uses a first observation (not claimed cold OS-cache latency), followed by three shuffled passes. Indexing runs outside measured query time. Report p50/p95 client wall time, indexing wall time, sampled process RSS, index file bytes, chunk and graph counts. Other workstation activity remains a latency confounder.

## Selection

Select using tuning data only. First prefer treatments with aggregate success@5 at least baseline and no more than one extra failed exact-symbol query per lane. Maximize equally weighted mean success@5 over lexical and graph lanes; balanced query types make this a macro average. Break ties by mean reciprocal rank@10, then returned compact tokens, then index size. If no enriched treatment improves measured retrieval, retain baseline retrieval and assess presentation improvements independently. Smaller chunk windows and larger context are not assumed better.

After selection, run baseline and the selected treatment on held-out queries once each. Report paired wins/losses and per-type outcomes. Do not retune using held-out failures. Best means best within this grid, frozen corpus and small query set, not a universal optimum.

## Preserved evidence

Keep source archive and checksum manifest, dataset checksum, binary checksum, exact environment settings, daemon logs, status/evidence endpoints, raw responses, per-query metrics and selection record. Stop only the daemon processes created by this experiment. Do not install a binary, modify production indexes, or merge experimental changes.
