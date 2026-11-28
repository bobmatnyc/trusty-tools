Added

- A full rebuild (first build or forced reindex) stamps the embedder that produced the index's vectors (model, dimension, precision) into the corpus `_meta` table. An incremental reindex keeps the existing stamp (Refs #10664).
- A load compares the stamp exactly with the running embedder and logs one WARN on a mismatch or an unreadable stamp. The index still loads; nothing is re-embedded or deleted. `GET /indexes/:id/status` reports `embedder_identity` with `status` set to `match`, `mismatch`, `unknown` (no stamp, such as an index built before this change), `unreadable` or `active_unknown` (Refs #10664).
