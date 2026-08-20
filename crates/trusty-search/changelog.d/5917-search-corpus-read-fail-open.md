Fixed

- `POST /indexes/{id}/search` no longer answers an index whose durable corpus
  cannot be read with an empty result set at HTTP 200. The in-memory chunk map
  and BM25 corpus are a cache of that corpus; idle eviction empties them, the
  rehydrate that would refill them read the same broken corpus and reported its
  failure only to the log, and `fetch_chunks_for_ids` did the same before
  falling back to those now-empty maps. An index holding 85,269 chunks returned
  `results: []` with `bm25_lane_degraded: true` — a flag that means "still
  warming up", which is the one thing this state is not — and the workaround was
  to delete and recreate the index. A failed durable read is now recorded on the
  index and the search refuses with `503 index_corpus_unavailable`, naming the
  index and the underlying fault. The record clears on the next successful read,
  so a transient failure does not wedge the index. Refs #5917.
- The sibling surfaces that read the same corpus refuse it too, instead of
  answering. `POST /indexes/{id}/grep` and `POST /grep` derive their file set
  from the chunk corpus, so an unreadable one answered
  `{"matches": [], "total": 0}` — "this literal is nowhere in your code" for a
  corpus that was never scanned. `GET /indexes/{id}/call_chain` resolves its
  entry point against the same snapshot and answered
  `404 entry point not found` for a symbol that exists. All three now return the
  same `503 index_corpus_unavailable`. The global `POST /search` fan-out still
  answers `200` so one broken index cannot fail the sweep, but now reports the
  index it dropped as `corpus_read_failed_indexes_skipped`. Refs #5917.
- The two producers of `index_corpus_unavailable` carry one field set: the
  open-failure body (#4087) gained `retryable` beside its `transient`, and the
  read-failure body (#6043) gained `failure_kind: "read_failed"` and `transient`
  beside its `retryable`. Refs #5917.
