Removed

- `CodeIndexer::embed_deferred_chunks` — folded into `embed_deferred_chunks_gated(progress_tx, pause)`, which does the same work and takes an optional pause gate. Pass `pause: None` for the previous behaviour. Keeping both left a second, unguarded entry point to a durable write, which `scripts/check_teardown_guard.sh` flags (#6524, #3049).
