Added

- The daemon evicts idle builder-slot directories once the volume holding
  `builders.slot_pool_root` reaches `builders.slot_pool_evict_pct` (default
  85%, always below the `disk.max_usage_pct` worktree guard). It removes
  whole `slot-N` directories oldest-first, and spares a slot whose lease is
  held, whose dead holder's build still runs, whose record cannot be read,
  whose `.cargo-lock` is held, or whose seed is staging. It refuses to
  sweep, logs a warning and deletes nothing when the pool root is `/`, the
  home directory, an ancestor of home, or does not resolve. Daemon
  shutdown stops a sweep at the next slot boundary. The sweep runs every
  10 minutes; a pass longer than that logs a warning.
  `TRUSTY_MPM_SLOT_POOL_EVICT=0` turns it off and
  `TRUSTY_MPM_SLOT_POOL_EVICT_INTERVAL_SECS` sets the cadence.
- `tm doctor` gains a `slot_pool_budget` row: the pool's slot count and its
  volume's usage against the eviction threshold.
