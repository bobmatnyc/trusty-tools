Fixed

- MCP `search`, `search_lexical`/`search_semantic`/`search_kg`/`search_all`, `grep`, and `index_status` against a worktree that has never been indexed now return a retryable `INDEX_NOT_READY` error carrying the state, reason, and a `grep`/`find` fallback, instead of the daemon's permanent-sounding `404 unknown index` (#4715).
