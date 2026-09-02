Changed

- `service::rpc::release_stores` is a plain drop again. The `Arc::strong_count`
  poll it grew in #6595 waited for connection tasks to release the router before
  the socket unlink; `serve_until_idle` now performs that drain itself on the
  shutdown path (#6601), so keeping the caller-side loop would be two
  implementations of one guarantee.
- `serve_options` inherits `RpcServeOptions::shutdown_drain` from the shared
  default (`shutdown::plannable_grace()`). An override to this crate's 3 s
  `SHUTDOWN_FLUSH_TIMEOUT` was reverted before release: it rested on the
  supervisor SIGKILLing this server at `ANALYZE_SIGTERM_PATIENCE`, which no
  supervisor path does. A bound analyze child is detached, so `ensure_running`
  never enters it in the supervised population and no reap path reaches it;
  `trusty-analyze stop` sends SIGTERM, polls 5 s and only reports. The 3 s drain
  averted no SIGKILL and abandoned the #6595 guarantee — every redb handle
  released before the socket unlink — three seconds into a multi-minute
  `analyze.review`.
- `SHUTDOWN_FLUSH_TIMEOUT` rises from 1 s to 3 s and is now an alias of
  `trusty_common::uds::ANALYZE_SHUTDOWN_FLUSH` rather than a second literal
  asserted equal to it. It bounds the supervisor's spawn-failure kill — the one
  path that signals an analyze child — leaving 2 s of the 5 s patience for the
  socket unlink, the redb store drop and exit.
