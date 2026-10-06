Fixed
- Vector search no longer re-reads every `VECTOR_KEYS` and `DELETED_VECTORS` row on each call. `HnswStore` caches the reverse map and tombstone set and rebuilds them only after a write commits; the cache is shared by every store open on the same palace file, so a write through one handle reaches the others (#9141).
- A recall's hit-log rows are written in one redb commit instead of one commit per hit (`RecallLog::record_batch`). A failed hit-log write is now logged at `warn` with its row count instead of being discarded; the recall still answers (#9141).
