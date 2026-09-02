Fixed

- `uds::server::serve_until_idle` now drains in-flight connections on
  SIGTERM/SIGINT before returning `ServeExit::Shutdown` (#6601). It used to
  return the instant the signal resolved, so the caller's socket unlink ran while
  handlers still held their `Arc<RpcRouter>` clones — and through them whatever a
  service's handlers own. In `trusty-analyze` that is a redb `Database`, and the
  unlink is exactly the signal a client acts on by spawning a successor, which
  then died on `Database already open. Cannot acquire lock.` (#6595).
- Connections are now counted whether or not an idle policy is configured, so
  every service behind this loop gets the guarantee rather than the one caller
  that noticed and polled `Arc::strong_count` itself.
- A client that dials during the drain is accepted and immediately closed, which
  reaches it as `UdsRpcError::NoResponse` rather than sitting in the kernel
  backlog until the listener drops and resets it.
- New `RpcServeOptions::shutdown_drain` bounds that wait, defaulting to
  `shutdown::termination_grace()` — the process's own SIGTERM-to-SIGKILL window,
  including a `TRUSTY_TERMINATION_GRACE_SECS` override. A handler that outlives
  it warns and the loop returns anyway.
