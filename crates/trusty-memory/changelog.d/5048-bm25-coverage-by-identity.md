Fixed

- BM25 backfill establishes coverage by drawer id, not by document count.
  `stats.doc_count >= drawer_count` was satisfied by a corpus carrying documents
  for drawers the palace no longer has, so `fully_indexed()` returned `true` and
  the startup sweep logged `incomplete=0` over a palace it had never indexed.
  `BackfillReport::fully_indexed()` is now `missing_after == Some(0)` — a
  verified, empty missing set — and no status can satisfy it on its own. A
  coverage probe that could not run reports `None` and logs at `error!`.
