Added

- `uds::server::request_peer_pid()` returns the kernel-reported pid of the
  process whose request a socket handler is serving, or `None` outside a
  socket dispatch. A handler can bind a privilege to the caller's process
  instead of a caller-written parameter (#8531).
