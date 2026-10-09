Added
- `memory_rpc::check_memory_protocol_at` and `ensure_memory_protocol_at` ask a
  trusty-memory daemon for its `protocol_version` and refuse one outside
  `SUPPORTED_MEMORY_PROTOCOLS` with the named `MemoryProtocolError`. A daemon
  that predates the handshake reads as `MemoryProtocol::PreHandshake` while
  protocol 1 is supported, and is reported once per process; any other failed
  check is an error
  ([#9288](https://github.com/bobmatnyc/trusty-tools/issues/9288)).
