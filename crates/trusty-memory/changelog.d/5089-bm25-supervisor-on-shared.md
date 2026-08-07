Changed

- `Bm25Supervisor` is now a thin face over `trusty-common`'s
  `uds::supervisor::UdsServiceSupervisor` (#5089). Its public API is unchanged —
  same constructors, same `ensure_running(palace, data_dir)`, same counters —
  and every behaviour it had is preserved by the shared implementation. What
  stays here is what is genuinely BM25's: the `TRUSTY_BM25_*` knobs, the socket
  path convention, the daemon's argv, and its two timing numbers. Those two are
  now passed in as a `ServiceTimeouts` value rather than being module constants:
  the 3 s spawn budget is justified by BM25 having no model to load, and the 5 s
  SIGTERM patience by the daemon's own 2 s `SHUTDOWN_FLUSH_TIMEOUT`. The
  compile-time guard on that relationship survives as the `BM25_TIMEOUTS` const
  item — `ServiceTimeouts::new` is a `const fn` that asserts it, so lowering the
  patience below the daemon's flush budget still fails the build
- The daemon binary is now located lazily, inside the spawn-spec closure, so the
  external-mode, already-running and socket-adoption paths no longer require
  `trusty-bm25-daemon` to be installed at all
- Unit coverage for the shared machinery — the LRU cap, the RSS ceiling, the
  three-state socket probe, the eviction bookkeeping — moved to
  `trusty-common`'s `uds/supervisor/tests.rs` with the code it covers. Every
  assertion has a counterpart there; nothing was dropped
