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
- New `RpcServeOptions::shutdown_drain` bounds that wait. It defaults to
  `shutdown::plannable_grace()` — the SIGTERM-to-SIGKILL window MINUS the new
  shared `shutdown::CLEANUP_RESERVE` — so the caller's post-serve work still has
  a budget when the drain runs long. `trusty-memory` runs its BM25 exit flush
  after `serve_until` returns, and a drain sized to the whole window would have
  spent the time that flush needs. A handler that outlives the budget warns,
  naming the socket, and the loop returns anyway.
- A service whose real SIGKILL deadline is shorter than the process grace window
  sets `shutdown_drain` explicitly rather than inheriting the default;
  `trusty-analyze` does, because its supervisor's `sigterm_patience` is what
  actually applies to it.
- `shutdown::CLEANUP_RESERVE` and `shutdown::plannable_grace` are new, and
  `trusty-search`'s `ShutdownBudget` now subtracts through them instead of
  keeping its own copy of the same 5 s policy.
