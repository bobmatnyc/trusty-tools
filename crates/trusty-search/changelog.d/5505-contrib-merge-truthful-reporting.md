Fixed

- `POST /indexes/{id}/graph` no longer reports success for a contribution that
  is not queryable (#5505). When the contributed-overlay merge failed, the
  endpoint answered `200` with `replaced: true` and graph totals that excluded
  the contribution just ingested, while queries silently returned incomplete
  results. It now answers `503 contrib_not_merged` with `persisted: true` — the
  contribution is durable, so the next successful rebuild restores it.
- That 503's `retryable` is earned rather than assumed. One unreadable
  `kg_contrib` row fails the whole load, so the response now names it in
  `blocking_producer` and reports `retryable: false` when the blocker is
  ANOTHER producer's row — retrying that ingest would fail identically forever.
  It stays `true` when the blocker is the caller's own row (a re-send replaces
  it) or when no single row is implicated.
- A reindex whose contributed-overlay merge fails no longer reports the graph
  stage `Ready`. The rebuild installs no graph in that case, so `Ready` would
  have described the pre-reindex graph as the run's product; the stage is now
  `Failed` with the reason, and `kg_complete` / `complete` carry
  `kg_contrib_merge_error`. The lexical and semantic stages are untouched.
- A failed contributed-overlay load no longer replaces the serving symbol graph
  with a derived-only one, and a lost (panicked or cancelled) save/merge worker
  no longer replaces it with an EMPTY graph. Both now install nothing and keep
  serving the previous graph until a rebuild succeeds.
- A failed derived-KG persist keeps merging (the in-memory graph is complete)
  but is now reported: the ingest response carries
  `derived_graph_persist_degraded` instead of only writing a log line.
