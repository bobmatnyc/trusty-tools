Changed

- `DaemonLock` gains a `socket_path` field recording the socket the writing
  daemon serves; `addr` is unchanged. The field is `#[serde(default)]`, so a
  lock written by a pre-#6288 daemon still parses, with an empty `socket_path`
  meaning HTTP only. `daemon::lock::write_lock` and
  `core::daemon_identity::write_lock` / `write_lock_at` take the socket path as
  a second argument
  ([#6288](https://github.com/bobmatnyc/trusty-tools/issues/6288)).
- `daemon::serve_http` waits for the shutdown signal through
  `trusty_common::shutdown_signal` and fans it out to both listeners, replacing
  a private line-for-line copy of that helper.
