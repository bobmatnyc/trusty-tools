Fixed

- The controller's per-project socket loop (`ctrl::socket_listener`) and the
  message bus's `accept_loop` now accept through
  `trusty_common::uds::accept_sized` instead of `UnixListener::accept`
  ([#6940](https://github.com/bobmatnyc/trusty-tools/issues/6940)). Linux builds
  the server-side AF_UNIX socket from scratch and does not copy the listener's
  `SO_SNDBUF`/`SO_RCVBUF` onto it, so both loops were serving sockets at
  `net.core.wmem_default` however `bind_hardened` had sized the listener —
  paying, on one end of every connection, the write-then-drain round trips
  [#6896](https://github.com/bobmatnyc/trusty-tools/issues/6896) exists to
  remove. `accept_sized` shipped in
  [#6942](https://github.com/bobmatnyc/trusty-tools/pull/6942) but converted
  only trusty-common's own call sites. Throughput only; no wire or API change.
