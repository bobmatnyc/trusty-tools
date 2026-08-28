Changed

- The bodies of the legacy session, hook, and polled-event routes moved from
  `daemon::api` into `daemon::rpc::sessions_legacy_ops`, and the axum handlers
  now delegate to them, so one route has one implementation across both
  transports. The handler signatures, paths, status codes, and response bodies
  are unchanged ([#6288](https://github.com/bobmatnyc/trusty-tools/issues/6288)).
- `daemon::api::session_start_correlation` and its two hook handlers are
  `pub(crate)` rather than `pub(super)`, so the shared `ingest_hook` body can
  reach them from `daemon::rpc`.
