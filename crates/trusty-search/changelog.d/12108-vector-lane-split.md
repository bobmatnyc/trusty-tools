Added

- The `search` log line and `meta.timings` split `vector_ms` into
  `vector_lock_ms` (graph lock wait), `vector_traverse_ms` (HNSW traversal,
  where mmap page fault-in lands) and `vector_map_ms` (key-to-chunk-id
  mapping), plus `hnsw_view` for a traversal on the mmap view. Ranking and
  latency are unchanged (Refs #12108).
