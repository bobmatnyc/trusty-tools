Fixed

- `Bm25Supervisor` now bounds the daemon population. Nothing but `shutdown` ever
  removed an entry from its map, so one cross-palace `memory_recall_all` over
  ~99 palaces left 99 child processes resident for the daemon's lifetime — and
  each one's memory scales with its palace's drawer text. Two limits now apply
  on every `ensure_running`: a cap on concurrently-live daemons with
  least-recently-used reaping (`TRUSTY_BM25_MAX_DAEMONS`, default 3), and a
  per-daemon RSS ceiling compared against a real measurement rather than merely
  declared (`TRUSTY_BM25_RSS_LIMIT_MB`, default 512, `0` disables). Spawns are
  serialised so a burst fan-out cannot satisfy the cap per-caller and violate it
  in aggregate. An unmeasurable RSS never reaps.
