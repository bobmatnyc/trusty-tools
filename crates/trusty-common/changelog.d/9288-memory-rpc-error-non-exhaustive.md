Breaking
- `memory_rpc::MemoryRpcError` gains a public `data` field, which carries the
  JSON-RPC error `data` member the client used to drop, and is now
  `#[non_exhaustive]`: a struct literal outside trusty-common no longer
  compiles, and a pattern needs `..`. The new `MemoryProtocolInfo` is
  `#[non_exhaustive]` too; build it with `MemoryProtocolInfo::new`
  ([#9288](https://github.com/bobmatnyc/trusty-tools/issues/9288)).
