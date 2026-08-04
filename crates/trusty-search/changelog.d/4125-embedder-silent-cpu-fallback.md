Fixed

- `/health` now reports `status: "degraded"` when the embedder permanently
  failed to reach its configured backend — `embedder_bootstrap: "failed"` (the
  graceful Python/MPS bootstrap gave up for this daemon's lifetime) or
  `"fell_back_to_ort"` (the swap-back watchdog abandoned a dead sidecar). Both
  previously sat next to `status: "ok"` forever, so a silent MPS → CPU
  performance regression was invisible to every monitor. `embedder: "ready"` is
  unchanged and still describes the currently-active backend (#4125)
- The graceful Python bootstrap's readiness probe now gets a larger budget on
  each retry instead of the same flat `TRUSTY_EMBEDDERD_STARTUP_TIMEOUT_SECS`
  twice. A cold torch import + model load, racing the daemon's own warm-boot,
  could exceed the flat 30 s on both attempts and permanently abandon a healthy
  sidecar (#4125)
