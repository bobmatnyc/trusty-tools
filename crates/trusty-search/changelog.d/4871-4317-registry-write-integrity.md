Fixed

- An unparseable `indexes.toml` is now an error rather than an empty registry.
  It previously read back as "no index was ever registered", and the next write
  published that view — overwriting a whole registry with a single entry, which
  is the mass-deregistration both #4317 (73 → 31, then 42 → 5) and #4871
  recorded. The corrupt file is left intact for recovery.
- Registry mutations are serialized process-wide and stage through a
  per-write temporary file instead of one shared `indexes.toml.tmp`. Concurrent
  writers previously interleaved: a registration landing between another task's
  load and its save was silently discarded (the observed 80 → 88 → 80 revert),
  and two writers racing on the shared temp file could publish a spliced,
  unparseable registry.
- The boot orphan-reaper removes orphans by id instead of republishing a
  pre-boot snapshot of the survivors, so an index registered while the sweep
  was deciding is no longer erased by the cleanup.

Known limitation: the fail-closed parse guarantees the WRITE path only. Read-only
callers that swallow the load error (`reindex/runner.rs`, `warm_boot/mod.rs`,
`server/tickers.rs`) still treat a corrupt registry as empty, exactly as before —
no regression, but the guarantee does not extend to them. See #4871.
