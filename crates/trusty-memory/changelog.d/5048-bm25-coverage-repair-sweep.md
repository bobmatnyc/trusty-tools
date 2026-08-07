Added

- Periodic BM25 coverage repair sweep (`bm25_repair`). The write path drops on a
  full queue, and the only thing that repaired a drop was the next daemon
  restart. A dropped enqueue, a failed index call, and an unverified startup
  sweep now queue the palace, and the sweep re-runs the lossless backfill on an
  interval (`TRUSTY_BM25_REPAIR_INTERVAL_SECS`, default 300s, `0` disables). A
  palace whose coverage is still unverified stays queued.
