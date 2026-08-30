Added

- **`PalaceHandle::remember_with_options_within`** — the same write with an explicit ceiling on its critical section, so a caller with its own SLA (and the concurrency tests) can set one without mutating process-wide env. `remember_with_options` delegates to it with `write_pipeline_timeout()` ([#6366](https://github.com/bobmatnyc/trusty-tools/issues/6366))
- **`write_pipeline_timeout()` and `slow_write_warn_threshold()`** in `memory_core::timeouts` — the ceiling on one write's critical section and the elapsed time above which a completed write is logged as slow ([#6366](https://github.com/bobmatnyc/trusty-tools/issues/6366))
