Added
- Per-index searches report where their time went (#10195). The `search` log line and the new `meta.timings` reply object carry `resolve_ms` (index lookup, including a cold lazy load), `lock_ms`, `embed_ms`, `vector_ms`, `lexical_ms`, `ripgrep_ms`, `fusion_ms`, `rehydrate_wait_ms`, `total_ms`, and whether the query started a corpus rehydrate (`rehydrate_ran`) or ran while the vector index promoted its mmap view (`hnsw_promoted`).
- `latency_ms` now starts before the index is resolved, so a search that lazy-loads a cold index counts the load.
- A search cancelled at the query deadline logs `query_timeout` at WARN with `index_id`, the truncated `query`, the `stage` it was in and `elapsed_ms`.
- `search_all` writes one `info` line per call (`search_all`) naming its target index ids, the indexes searched, `embed_ms` and `latency_ms`; it logged only at debug before.
- The rehydrate log line adds `index_id` and the stage split `scan_ms`, `sort_ms`, `entities_ms`, `bm25_build_ms` and `commit_ms` beside `rehydrate_ms`.
