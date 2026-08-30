Added
- `GET /indexes?details=true` and the `console_metrics` tool report `last_used_unix` per index — `max(last_queried_unix, last_indexed_unix)` off `indexes.toml`, absent for an index never searched or indexed. The trusty-console index roster renders it as a Last Used column and sorts by it (#6424).
