Performance

- **`GET /api/v1/status` and `GET /api/v1/palaces` no longer force-open every
  palace on disk (issue #4637).** Both handlers looped over the whole registry
  calling `PalaceRegistry::open_palace` — a synchronous, blocking per-palace
  load (usearch vector index, KG redb, full drawer table, recall-log redb) —
  inline on the async axum executor, with no `spawn_blocking`, no timeout and
  no pagination. Against the live daemon's 5,794 palaces and the 64-slot LRU
  (`DEFAULT_MAX_OPEN_PALACES`), ~98.9% of those were cold opens at ~0.9–1.1 s
  each: roughly 87–106 minutes of disk I/O per request. Measured before the
  fix, `/api/v1/status` did not respond within 90 s while `/health` returned in
  36 ms. Both routes now read counts through `PalaceRegistry::peek` — a
  cache-only, zero-I/O, non-promoting lookup — carrying the fix from issue
  #1924 (which fixed the same anti-pattern in the MCP `console_metrics`
  handler) across to the HTTP handler path, where it had never been applied.
  The same conversion lands on the chat-surface twins `list_palaces` and
  `get_status`, and the `PalaceRegistry::list_palaces` directory walk that
  feeds them now runs on the blocking pool.

- **Cross-palace recall and the dream cycle no longer park a tokio worker
  thread (issue #4637).** `recall_all` (`GET /api/v1/recall`), its chat and MCP
  twins, and `dream_run` (`POST /api/v1/dream/run`) genuinely need every palace
  open — a recall answered from cache-resident palaces only would silently omit
  ~98.9% of the corpus, and a dream cycle that skipped uncached palaces would
  silently stop maintaining them — so these are deliberately *not* converted to
  `peek()`. Their blocking open loops moved to `spawn_blocking` instead, which
  keeps the semantics intact while taking the multi-minute stall off the async
  executor. Three byte-identical copies of that loop collapsed into one shared
  `open_palaces_blocking` helper. These routes are still slow by nature; making
  them fast needs a cross-palace index or an explicit palace scope, not a
  cache-only read.
