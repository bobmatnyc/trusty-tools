# trusty-mcp

Shared JSON-RPC 2.0 / MCP protocol primitives for the trusty-* ecosystem.

Every trusty-* MCP server speaks the same line-delimited JSON-RPC over stdio and
advertises the same `rpc.discover` document. This crate holds that one
implementation so a parse-error fix or a `serverInfo` change lands once.

## What's here

| Module | Surface |
|---|---|
| (crate root) | `Request`, `Response`, `JsonRpcError`, `error_codes`, `initialize_response`, `run_stdio_loop` |
| `service` | `ServiceDescriptor` — the registration contract a linked service implements to contribute tools to a merged OpenRPC document |
| `openrpc` | `OpenRpcBuilder`, `discover_response` — build the `rpc.discover` manifest |
| `daemon_bridge` | `DaemonBridgeConfig`, `ensure_daemon_up` — probe/spawn/poll a service's HTTP daemon before entering the dispatch loop |
| `single_flight` | `StartLock`, `ensure_daemon_up_single_flight` — `flock(2)`-guarded variant so N concurrent bridge processes start exactly one daemon (#1152) |

## History

These types lived in `trusty_common::mcp` (and, before that, in a standalone
`trusty-mcp-core`) until [ADR-0040](../../docs/adr/0040-trusty-mcp-services-absorbs-trusty-gworkspace.md)
extracted them again. `memory_rpc` did not come along: it resolves the
trusty-memory daemon's address through `trusty_common::daemon_addr`, so it
stays in `trusty-common` as `trusty_common::memory_rpc`.

There is deliberately no re-export shim in `trusty-common` —
`trusty_common::mcp` no longer exists.

## Testing

```bash
cargo test -p trusty-mcp
```
