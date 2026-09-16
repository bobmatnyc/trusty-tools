Added

- `GET /indexes/:id/status` reports `migration_error`, the set of outstanding per-stage migration faults (`stage`, `detail`, `at`), and `search_health` returns a new `index_migration_failed` verdict instead of the `index_empty` one that prescribed a reindex. A failed `chunks.json` → `index.redb` migration, or a failed M001–M005 chain, was visible only as one WARN line while both endpoints rendered the index as ordinarily empty. Faults are keyed by stage, so a schema chain with nothing to do no longer clears a still-true JSON migration fault (#7979).
