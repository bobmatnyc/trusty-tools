Fixed
- Compacting an oversized pre-fix `errors.jsonl` no longer loses records that other processes append while it runs. The live file is renamed aside first, so new appends start a fresh file, and bytes written through an already-open descriptor are copied into the compacted slot. When the compaction fails, the whole file moves into `errors.jsonl.1` instead; no failure path deletes a record.
- `ErrorStore::append` no longer holds the store lock while it waits for the rotation lock or writes to disk, and that wait is capped at 50 ms instead of 2 s. A stuck rotation-lock holder no longer delays every other ERROR event and every reader of the store.
- Reading a store while it rotates no longer returns the same records twice, so the multi-store bug-report view no longer double-counts them.
