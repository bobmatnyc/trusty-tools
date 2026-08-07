Added

- Periodic BM25 coverage repair sweep (`bm25_repair`). The write path drops on a
  full queue, and the only thing that repaired a drop was the next daemon
  restart. A dropped enqueue, a failed index call, and an unverified startup
  sweep now queue the palace, and the sweep re-runs the lossless backfill on an
  interval (`TRUSTY_BM25_REPAIR_INTERVAL_SECS`, default 300s, `0` disables). A
  palace whose coverage is still unverified stays queued.
- The startup sweep enumerates palaces from disk (`list_palaces` + `open_palace`)
  instead of `registry.list()`, which snapshots only currently-open handles and
  is capped at 64. On a ~99-palace host at least 35 were never probed, never
  queued, and the sweep still logged `all coverage verified`. Palaces that
  cannot be enumerated, opened, or verified are now counted and queued, and a
  sweep that enumerated nothing can no longer read as complete.
- The repair pass resolves palaces with `open_palace`, which hydrates. A dirty
  palace that went idle and was evicted was previously dropped from the queue
  permanently, so its gap waited for a restart.
