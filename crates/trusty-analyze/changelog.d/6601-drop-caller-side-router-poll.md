Changed

- `service::rpc::release_stores` is a plain drop again. The `Arc::strong_count`
  poll it grew in #6595 waited for connection tasks to release the router before
  the socket unlink; `serve_until_idle` now performs that drain itself on the
  shutdown path (#6601), so keeping the caller-side loop would be two
  implementations of one guarantee. The shutdown budget is the shared
  `RpcServeOptions::shutdown_drain` — the process's termination grace — rather
  than this crate's 1 s `SHUTDOWN_FLUSH_TIMEOUT`, which was too short for an
  `analyze.review` handler that runs for minutes.
