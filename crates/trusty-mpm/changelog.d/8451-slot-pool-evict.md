Added

- The daemon evicts idle builder-slot directories once the volume holding
  `builders.slot_pool_root` reaches `builders.slot_pool_evict_pct` (default
  85%, always below the `disk.max_usage_pct` worktree guard). It removes
  whole `slot-N` directories oldest-first, and spares a slot whose lease is
  held, whose dead holder's build still runs, whose record cannot be read,
  whose `.cargo-lock` is held, or whose seed is staging. The sweep runs
  every 10 minutes; `TRUSTY_MPM_SLOT_POOL_EVICT=0` turns it off and
  `TRUSTY_MPM_SLOT_POOL_EVICT_INTERVAL_SECS` sets the cadence.
- `tm doctor` gains a `slot_pool_budget` row: the pool's slot count and its
  volume's usage against the eviction threshold.
