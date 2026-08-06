Fixed

- The `in_flight` gauge reported by `GET /status` no longer leaks. Both review
  call sites bracketed `run_review(...).await` with a bare `fetch_add` /
  `fetch_sub` pair, so the decrement was skipped when an HTTP client
  disconnected mid-review (axum drops the handler future) or when the pipeline
  panicked — either one inflating the counter permanently for the life of the
  process. Both sites now hold an `InFlightCountGuard` whose `Drop` decrements,
  matching the RAII discipline already used for the dedup slot
  ([#5026](https://github.com/bobmatnyc/trusty-tools/pull/5026)).
