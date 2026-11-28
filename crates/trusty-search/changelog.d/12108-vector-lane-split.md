Added

- The `search` log line and `meta.timings` split `vector_ms` into
  `vector_lock_ms` (graph lock wait), `vector_traverse_ms` (HNSW traversal,
  where mmap page fault-in lands) and `vector_map_ms` (key-to-chunk-id
  mapping), plus `hnsw_view` for a traversal on the mmap view. Ranking and
  latency are unchanged (Refs #12108).
- The same line and `meta.timings` split `fusion_ms` into `chunk_fetch_ms`
  (every corpus chunk fetch, from submit to the blocking pool to the result),
  `chunk_fetch_queue_ms` (the blocking-pool queue wait inside it), `kg_ms`
  (KG expansion) and `rerank_ms` (the MMR pass). `fusion_ms` stays. Ranking
  and latency are unchanged (Refs #11964).
