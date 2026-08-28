Added

- The daemon binds a hardened Unix socket at `<data dir>/trusty-mpm.sock`
  alongside `127.0.0.1:7880`, through the same
  `trusty_common::uds::bind_singleton_hardened` entry point trusty-memory,
  trusty-review, and trusty-analyze use — a `0600` socket in a `0700` directory,
  with a peer-uid check on every accepted connection. The listener serves an
  `RpcRouter` with no registered methods, so every request answers
  `method_not_found` until slice 2 moves the first route across; both listeners
  drain on the same SIGTERM/SIGINT and the socket file is unlinked before the
  listener is dropped. A socket that cannot be bound fails startup with an error
  naming the path rather than degrading to an HTTP-only daemon. No HTTP route
  moved and nothing was removed (slice 1 of
  [#6288](https://github.com/bobmatnyc/trusty-tools/issues/6288), ADR-0032).
- `daemon::socket` — `socket_path`, `bind`, and `serve_until_shutdown`, the
  listener's bind and serve halves, split so a test can drive the real path with
  its own shutdown future.
