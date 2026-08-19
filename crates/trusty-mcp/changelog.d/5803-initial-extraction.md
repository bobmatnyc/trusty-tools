Added

- **New crate: the JSON-RPC 2.0 / MCP protocol primitives every trusty-* MCP server shares** — `Request`, `Response`, `JsonRpcError`, `error_codes`, `initialize_response`, `run_stdio_loop`, `ServiceDescriptor`, `OpenRpcBuilder`/`discover_response`, `DaemonBridgeConfig`/`ensure_daemon_up`, and `StartLock`/`ensure_daemon_up_single_flight`. These moved verbatim out of `trusty_common::mcp`; the code is unchanged apart from import paths, so no behaviour differs from trusty-common 0.35.1 ([#5803](https://github.com/bobmatnyc/trusty-tools/issues/5803), ADR-0040)
  - `single_flight` moved with `daemon_bridge` even though it post-dates ADR-0040's extraction table — it landed in #5750 and has zero non-`mcp` coupling
  - `memory_rpc` deliberately stayed in `trusty-common`: it resolves the trusty-memory daemon through `trusty_common::daemon_addr` and `trusty_common::data_dir`
