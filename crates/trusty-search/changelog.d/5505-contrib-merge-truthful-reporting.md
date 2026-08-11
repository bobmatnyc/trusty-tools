Fixed

- `POST /indexes/{id}/graph` no longer reports success for a contribution that
  is not queryable (#5505). When the contributed-overlay merge failed, the
  endpoint answered `200` with `replaced: true` and graph totals that excluded
  the contribution just ingested, while queries silently returned incomplete
  results. It now answers `503 contrib_not_merged` with `retryable: true` and
  `persisted: true` — the contribution is durable, so the next successful
  rebuild restores it and the same document can simply be re-sent.
- A failed contributed-overlay load no longer replaces the serving symbol graph
  with a derived-only one, and a lost (panicked or cancelled) save/merge worker
  no longer replaces it with an EMPTY graph. Both now install nothing and keep
  serving the previous graph until a rebuild succeeds.
- A failed derived-KG persist keeps merging (the in-memory graph is complete)
  but is now reported: the ingest response carries
  `derived_graph_persist_degraded` instead of only writing a log line.
