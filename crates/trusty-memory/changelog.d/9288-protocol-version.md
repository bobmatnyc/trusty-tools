Added
- The daemon answers `memory.protocol` with a monotonic `protocol_version`
  (`transport::methods::protocol::PROTOCOL_VERSION`, now 1), pinned to its
  wire surface by a test. The CLI, the hooks and the `serve --stdio` bridge
  check it before calling the daemon and refuse a daemon from another release
  with `MemoryProtocolError`. A daemon that predates the handshake is still
  called, so a client installed before the daemon restarts keeps working
  ([#9288](https://github.com/bobmatnyc/trusty-tools/issues/9288)).
