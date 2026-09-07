Fixed

- `run_uds_accept_loop` now accepts through `trusty_common::uds::accept_sized`
  instead of `UnixListener::accept`
  ([#6940](https://github.com/bobmatnyc/trusty-tools/issues/6940)). Linux builds
  the server-side AF_UNIX socket from scratch and does not copy the listener's
  `SO_SNDBUF`/`SO_RCVBUF` onto it, so the daemon was serving its multi-KiB
  embedding frames over a socket at `net.core.wmem_default` however
  `bind_uds_listener` had sized the listener. `accept_sized` shipped in
  [#6942](https://github.com/bobmatnyc/trusty-tools/pull/6942) but converted
  only trusty-common's own call sites. Throughput only; no wire or API change.
