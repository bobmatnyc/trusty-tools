Fixed

- `GET /indexes/{id}/chunks` no longer reports a failed corpus read as an empty
  index. Both enumeration paths absorbed the failure: the cursor path turned a
  redb read error into an empty page with `next_cursor: null`, which a paging
  client reads as "corpus exhausted", and the offset path reported the in-memory
  chunk map's length as `total` even when a rehydrate had not committed. An
  index holding 50,929 chunks exported zero of them at HTTP 200, and
  trusty-analyze scored the empty corpus and published
  `complexity_distribution total: 0`. Both paths now return
  `503 index_corpus_unavailable` with `retryable: true`. The offset path waits
  out a slow rehydrate first, retrying on the same `REHYDRATE_RACE_RETRIES`
  budget the BM25 and grep lanes use (~27s at the defaults), so a large cold
  index that simply takes 27-40s to rehydrate serves its corpus rather than
  erroring. Refs #6043, #5917.
