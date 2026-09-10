Fixed

- `run_uds_socket_bind_failure_is_fatal` now bounds the daemon future it awaits
  at ten seconds. Its shutdown signal is `std::future::pending()`, so a daemon
  that manages to bind serves forever — which is what happened on Linux, where
  `bind_singleton_hardened` read the test's occupying regular file as a dead
  socket and bound over it. The test consumed the whole `Rust tests
  (pre-publish gate)` shard 6 until the job's 45-minute cancel, on every push to
  main, and the shard's other targets never reported. A regression now fails in
  seconds naming the cause ([#7312](https://github.com/bobmatnyc/trusty-tools/issues/7312),
  [#7219](https://github.com/bobmatnyc/trusty-tools/pull/7219))
