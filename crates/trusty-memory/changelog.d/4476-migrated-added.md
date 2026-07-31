Added

- **`/health` now reports live worker-pool occupancy (issue #4001).** The
  payload carries a `worker` block — `in_flight`, `oldest_age_secs`, and a
  `wedged` verdict — and the top-level `status` becomes `"wedged"` when the
  oldest in-flight palace operation has outlived
  `TRUSTY_WEDGE_THRESHOLD_SECS` (default: twice `open_queue_timeout()`).
  Backed by a new `worker_liveness` module: a fixed-size, lock-free slot table
  of operation start timestamps, registered from `open_palace_handle` (the
  choke point every `memory_recall` / `memory_remember` passes through, and
  where the #3992 wedge actually occurred). One CAS in and one store out per
  operation, no allocation and no syscall on the hot path, so the gauge cannot
  become the load problem it exists to detect. Registration is RAII, so the
  `?` and panic paths release it as reliably as the success path.
