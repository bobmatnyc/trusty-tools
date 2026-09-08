Added
- `uds::sse`, behind the `axum-server` feature: renders one framed JSON-RPC
  stream as the Server-Sent Events a browser reads — `sse_response`, `sse_tail`,
  `sse_data`, the 20-second `SSE_HEARTBEAT_INTERVAL` and the 64-item
  `SSE_BUFFER`. Hoisted verbatim out of `trusty-console`'s crate-private
  `uds_sse` so a second UI crate bridging a webview onto a daemon socket shares
  it rather than carrying a third copy
  ([#6637](https://github.com/bobmatnyc/trusty-tools/issues/6637)).
