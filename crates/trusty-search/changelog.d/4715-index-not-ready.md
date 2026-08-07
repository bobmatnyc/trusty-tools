Fixed

- MCP `search`, `search_lexical`/`search_semantic`/`search_kg`/`search_all`, `grep`, `index_status`, and `list_chunks` against a worktree that has never been indexed now return a retryable `INDEX_NOT_READY` error carrying the state, reason, and a `grep`/`find` fallback, instead of the daemon's permanent-sounding `404 unknown index` (#4715).
- `GET /indexes/:id/status`, `GET /indexes/:id/chunks`, and `POST /indexes/:id/grep` now return `503` rather than `404` for an index that is registered but not resident (cold-parked after a timed-out warm-boot restore, or permanently restore-failed). A `404` from these endpoints now means the same thing it has always meant on the search endpoint: no such index anywhere (#4715).
