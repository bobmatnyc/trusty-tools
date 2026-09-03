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
| `daemon_bridge_json_rpc` | `UdsBridgeConfig`, `DaemonBridgeJsonRpc`, `run_stdio_bridge` — the shared stdio↔UDS JSON-RPC forwarder (#6316). **Feature-gated**, see below |

## Features

| Feature | Default | What it turns on |
|---|---|---|
| `daemon-bridge-json-rpc` | off | The `daemon_bridge_json_rpc` module, and with it the crate's only dependency on `trusty-common` (its `uds` feature) |

Everything else in this crate is pure `serde` / `tokio` / `reqwest`. The
forwarder needs `trusty_common::uds`'s framed client, and that is a much heavier
edge than a crate every MCP server links should carry unconditionally — so it is
opt-in and the default rlib stays as lean as ADR-0040 left it. Prove the edge is
gated:

```bash
cargo tree -p trusty-mcp -e features -i trusty-common
# error: package ID specification `trusty-common` did not match any packages
cargo tree -p trusty-mcp -e features -i trusty-common --features daemon-bridge-json-rpc
# trusty-common v0.47.2 … └── trusty-mcp feature "daemon-bridge-json-rpc"
```

Two names sit close together and are not the same thing. `daemon_bridge` probes
and spawns a service's **HTTP** daemon before the dispatch loop starts.
`daemon_bridge_json_rpc` is the loop itself, forwarding each request to a daemon
over a **Unix socket**. A consumer can want either without the other.

The forwarder could not live here before [PR #6726](https://github.com/bobmatnyc/trusty-tools/pull/6726):
`trusty-common`'s `tickets` feature depended on `trusty-mcp`, so this edge would
have closed a cycle (#6316).

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
cargo test -p trusty-mcp --all-features --no-fail-fast
```

`--all-features` matters here: a bare run compiles `daemon_bridge_json_rpc` out
and its tests never execute (#4901's family).
