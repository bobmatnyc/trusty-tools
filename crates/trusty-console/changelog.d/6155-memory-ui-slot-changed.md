Changed
- The Server-Sent Events plumbing both UDS bridges use — the `data:` encoding,
  the 20-second keep-alive, the cancel-safe reader, the terminal error event —
  moved out of `search_uds::routes` into `uds_sse`, so the memory bridge shares
  it rather than carrying a second copy
  ([#6155](https://github.com/bobmatnyc/trusty-tools/issues/6155)).
- The router moved out of `server/mod.rs` into `server/router.rs`; the five new
  memory routes pushed that file past the 500-SLOC cap. `build_router` and its
  two siblings are re-exported, so no call site changed
  ([#6155](https://github.com/bobmatnyc/trusty-tools/issues/6155)).
