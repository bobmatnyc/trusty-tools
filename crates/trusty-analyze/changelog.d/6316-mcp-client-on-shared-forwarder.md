Changed

- **The MCP dispatcher dials the daemon through the shared `trusty_mcp::daemon_bridge_json_rpc`.** `mcp::rpc_client::call` built its own JSON-RPC frame, called `trusty_common::uds::send_framed_request_capped` and unpacked an `RpcResponse` by hand — the same transport trusty-memory's stdio bridge carried a second copy of. Both now run one implementation ([#6316](https://github.com/bobmatnyc/trusty-tools/issues/6316))
  - The 32 MiB response budget and `core::mcp_client_timeout()` are unchanged; they are passed to the bridge rather than to the client. No request rewriter: the dispatcher has already built the exact `params` each `analyze.*` method expects
  - `mcp::stdio::run` is untouched. This crate's MCP surface is a tool translator with its own `tools/list` and its own #917 response-size guard, not an envelope forwarder, so the stdio loop stays where it is
  - A transport failure and a daemon-side JSON-RPC error still both surface as `DispatchError::Transport` naming the failing method. The message now carries the daemon's error code as well: `<method> over <socket>: <message> (<code>)`
