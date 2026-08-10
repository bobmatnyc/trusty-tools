Fixed

- `GET /health` gained `indexes_populated`, `indexes_empty`, and `total_chunks`. `indexes` and `warmboot_summary.indexes_loaded` count registration slots, so a deployment where 220 of 222 indexes held zero chunks reported `indexes_loaded: 222/222`, `indexes_failed: 0`, `status: ok` while a consuming application returned empty context on 913 consecutive operations across 44 days. Counts come from the durable corpus (the in-memory map reads `0` after idle eviction) and exclude corpus-failed indexes, whose real count is unknown rather than zero (#4839).
