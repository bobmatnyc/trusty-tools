Added

- `GET /indexes/:id/status` reports a per-index `migration_error` (`stage`, `detail`, `at`) when a migration fails, and `search_health` relays it. A failed `chunks.json` → `index.redb` migration, or a failed M001–M005 chain, was visible only as one WARN line while both endpoints rendered the index as ordinarily empty (#7979).
