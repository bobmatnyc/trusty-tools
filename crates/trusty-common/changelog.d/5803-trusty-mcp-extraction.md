Breaking

- **`trusty_common::mcp` is gone, and there is no re-export shim** — the MCP protocol primitives now live in the new `trusty-mcp` crate. A consumer that used `trusty_common::mcp::{Request, Response, error_codes, initialize_response, run_stdio_loop, ServiceDescriptor, OpenRpcBuilder, ensure_daemon_up, ensure_daemon_up_single_flight, StartLock}` depends on `trusty-mcp` and imports `trusty_mcp::…` instead ([#5803](https://github.com/bobmatnyc/trusty-tools/issues/5803), ADR-0040)
- **The `mcp` feature is replaced by `memory-rpc`**, which gates what stayed: `trusty_common::memory_rpc`, the discovery-based JSON-RPC client for the trusty-memory daemon — formerly `trusty_common::mcp::memory_rpc`. It stayed because it resolves the daemon's address through `trusty_common::daemon_addr` rather than owning any wire types
- **`catchup` now implies `memory-rpc`** instead of `mcp`, and **`tickets` now pulls in `trusty-mcp`** for its stdio loop
