Breaking

- The daemon no longer binds `127.0.0.1:7879`. It serves JSON-RPC 2.0 over
  `<data dir>/trusty-analyze/trusty-analyze.sock`, which every consumer derives
  through `trusty_common::daemon_socket_path` rather than reading a
  written-down address (#6287, ADR-0032). The `http_addr` discovery file is
  gone with it. `serve --port` and `serve --mcp-port` are accepted, hidden, and
  ignored with a warning rather than removed: the launchd plist on every
  machine that installed before this change still passes `--port 7879`, a
  `cargo install` does not rewrite it, and clap exiting 2 under
  `KeepAlive::Always` is a permanent crash loop with nothing in the logs but a
  usage message. `serve --socket` overrides the derived path.
- `trusty-analyze port` is replaced by `trusty-analyze socket`. The path
  resolves whether or not a daemon is running, so the new command reports
  liveness too and exits non-zero when nothing answers — preserving the
  property that `$(trusty-analyze socket)` fails rather than handing a caller a
  path to a dead socket.
- `service::routes`, `service::ui` and the axum router are replaced by
  `service::rpc`. `service::events::DEFAULT_PORT` is removed, and
  `ApiErrorKind` replaces the `axum::http::StatusCode` the handlers reported
  through.
- `--mcp-port` and the `/sse` broadcast are DELETED, not ported. `/sse`'s only
  subscriber was this daemon's own SPA; `--mcp-port` had no in-repo consumer at
  all and was a second ADR-0032-forbidden HTTP surface.
- The embedded UI is not served by this daemon any more. `ui/dist` stays
  tracked; the console-hosted mount is follow-up work.
- `commands::daemon::handle_start` no longer takes a `socket` parameter. It
  probed the socket it was handed but always spawned a child that derived its
  own, so a non-default path would have been probed and reported while a
  different one was served. It resolves the single path itself now.
