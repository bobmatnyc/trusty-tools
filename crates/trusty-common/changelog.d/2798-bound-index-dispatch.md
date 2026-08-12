Fixed

- `search_index::index_files_best_effort` no longer spawns an unbounded number
  of detached OS threads
  ([#2798](https://github.com/bobmatnyc/trusty-tools/issues/2798)). Every
  incremental index batch now goes through one shared pool: at most 4 run
  concurrently, at most 64 more queue behind them. Against a degraded but
  reachable trusty-search daemon — where #2785's retry lets a single file take
  up to ~6.2s — threads used to pile up faster than they drained, with nothing
  pushing back. At saturation a batch is DROPPED rather than blocked or queued
  without limit: blocking would stall the agent task the fail-open contract
  exists to protect. The drop is logged at `warn` with the file count, the
  project root, the first path, and the running process-wide drop total, so a
  degraded daemon is visible instead of silently losing updates. The caller
  contract is unchanged — still non-blocking, still fail-open, still no public
  API change.
