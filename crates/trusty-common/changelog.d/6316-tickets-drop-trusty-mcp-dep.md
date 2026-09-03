Changed
- The `tickets` feature no longer depends on `trusty-mcp`. That edge closed a
  dependency cycle — `trusty-mcp` depends on this crate — and blocked the shared
  stdio↔UDS JSON-RPC forwarder ([#6316](https://github.com/bobmatnyc/trusty-tools/issues/6316)).
  The `tickets-mcp` binary now runs on a private, dependency-free JSON-RPC stdio
  loop in `tickets::stdio`; `trusty_mcp::run_stdio_loop` stays the shared loop
  for every MCP server outside this crate. The public API of
  `trusty_common::tickets` is unchanged — `server::run_stdio`,
  `server::handle_message` and `server::handle_tool_call` keep their signatures,
  and the wire behaviour (handshake, notification suppression, error codes) is
  byte-identical.
