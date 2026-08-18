Fixed

- `tm services status`, `list`, `port`, `url`, `health`, and `log` no longer
  panic before printing anything in a debug build
  (closes [#5965](https://github.com/bobmatnyc/trusty-tools/issues/5965)).
  `RealHttpProber::get_health` built a `reqwest::blocking` client directly on
  the caller's thread, and `tm`'s `main` is `#[tokio::main]` — so every health
  probe constructed reqwest's internal runtime inside the async runtime and
  aborted with "Cannot drop a runtime in a context where blocking is not
  allowed". Release builds only appeared healthy because the guard that raises
  that panic is compiled in under `debug_assertions`; the call still parked the
  thread it ran on from inside `poll`. The client now runs on a dedicated OS
  thread that is joined for its result, matching
  `trusty_common::search_index::best_effort_create_index`. `init` and `restart`
  were unaffected — they never probe.
