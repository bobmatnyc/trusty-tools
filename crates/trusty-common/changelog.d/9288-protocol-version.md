Added
- `memory_rpc::check_memory_protocol_at` and `ensure_memory_protocol_at` ask a
  trusty-memory daemon for its `protocol_version` and refuse one outside
  `SUPPORTED_MEMORY_PROTOCOLS` with the named `MemoryProtocolError`. A daemon
  that predates the handshake reads as `MemoryProtocol::PreHandshake`, and any
  other failed check is an error. `MemoryRpcError` now keeps the JSON-RPC
  error `data` member as `data`
  ([#9288](https://github.com/bobmatnyc/trusty-tools/issues/9288)).
