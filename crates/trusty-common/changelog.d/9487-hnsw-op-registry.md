Added
- `HnswStore::oldest_op` and `UsearchStore::oldest_hnsw_op` report the longest-running in-flight `upsert` or `search` (`HnswOp`, `HnswOpKind`). The entry is registered on the blocking thread, so it stays visible after the awaiting future is dropped by a timeout. `OpPark` (behind `embedder-test-support`) holds a call inside that section for tests (#9487).
