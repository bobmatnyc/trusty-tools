Changed
- The search console's Indexes roster flags an index whose vector store is
  empty. Its Status column showed `ready` — the reindex-queue state, not a
  health verdict — so an index holding 58,415 chunks and 0 vectors, which
  answers nothing for every vector query, rendered green. The column now shows
  `Degraded` with the fault sentence on hover, computed by the same
  `indexHealth` the expanded per-index panel's banner uses, so the row and the
  panel cannot word the same fault differently
  ([#6699](https://github.com/bobmatnyc/trusty-tools/issues/6699), follows
  [#6689](https://github.com/bobmatnyc/trusty-tools/issues/6689)).
- The roster draws from one `GET /indexes?details=true` call instead of
  `GET /indexes` followed by a `GET /indexes/{id}/status` per row — 42 requests
  down to 1 on a 41-index daemon, with the same per-index directory walk count
  and the same column values
  ([#6699](https://github.com/bobmatnyc/trusty-tools/issues/6699)).
