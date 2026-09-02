Changed

- `service::rpc::release_stores` is a plain drop again. The `Arc::strong_count`
  poll it grew in #6595 waited for connection tasks to release the router before
  the socket unlink; `serve_until_idle` now performs that drain itself on the
  shutdown path (#6601), so keeping the caller-side loop would be two
  implementations of one guarantee.
- `serve_options` sets `RpcServeOptions::shutdown_drain` explicitly, to this
  crate's own `SHUTDOWN_FLUSH_TIMEOUT`. Inheriting the shared default would size
  the drain to the process termination grace, which is not this server's
  deadline: ADR-0032 makes it on-demand under `UdsServiceSupervisor`, whose
  `ANALYZE_SIGTERM_PATIENCE` is 5 s, so the SIGKILL would land 5 s into a drain
  planned for ten times that.
- `SHUTDOWN_FLUSH_TIMEOUT` rises from 1 s to 3 s and is now an alias of
  `trusty_common::uds::ANALYZE_SHUTDOWN_FLUSH` rather than a second literal
  asserted equal to it. One second was the budget for an accept loop that
  returned immediately; three is the budget for one that waits for in-flight
  handlers, and it still leaves 2 s of the supervisor's patience for the socket
  unlink, the redb store drop and exit.
